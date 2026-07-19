mod cli;
mod consts;
mod decode;
mod encode;

use std::collections::HashSet;
use std::fs::{OpenOptions, create_dir_all, remove_file, rename};
use std::path::Path;
use std::sync::Mutex;

use anyhow::{Result, ensure};
use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use walkdir::WalkDir;

use crate::cli::{Cli, Codec};
use crate::encode::{Encode, ImageConfig, ImageFormat};

fn main() -> Result<()> {
    let params = Cli::parse();

    macro_rules! log {
        ($verbose:expr, $($args:expr),*) => {
            if (!$verbose || params.verbose) && !params.quiet {
                eprintln!($($args),*);
            }
        };
    }

    let target_format = match params.format.unwrap().to_ascii_lowercase().as_str() {
        "copy" => ImageFormat::Copy,
        "png" => ImageFormat::Png,
        "jpeg" => ImageFormat::Jpeg,
        "webp" => ImageFormat::Webp,
        "avif" => ImageFormat::Avif,
        _ => unreachable!(),
    };
    ensure!(
        !matches!(params.codec, Codec::Aac { .. } | Codec::XheAac { .. })
            || matches!(target_format, ImageFormat::Copy | ImageFormat::Jpeg | ImageFormat::Png),
        "MP4 format only accepts JPEG or PNG"
    );

    if let Some(n) = params.jobs {
        rayon::ThreadPoolBuilder::new().num_threads(n).build_global()?;
    }

    if !params.output.exists() {
        create_dir_all(&params.output)?;
        log!(false, "Created directory: {}", params.output.display());
    }

    // clean temp files
    WalkDir::new(&params.output)
        .into_iter()
        .flatten() // ignore error results
        .filter(|d| d.file_type().is_file())
        .map(|d| d.into_path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "tmp"))
        .try_for_each(|p| -> Result<_> { Ok(remove_file(p)?) })?;

    const FILE_EXTS: &[&str] = &["ncm", "flac", "mp3", "wav", "alac", "m4a", "aac"];
    let files: Vec<_> = WalkDir::new(&params.source)
        .into_iter()
        .flatten()
        .filter(|d| d.file_type().is_file())
        .map(|d| d.into_path())
        .filter_map(|path| FILE_EXTS.contains(&path.extension()?.to_str()?).then_some(path))
        .collect();

    let progress_bar = ProgressBar::new(files.len() as u64).with_style(
        ProgressStyle::with_template(
            "[{wide_bar}] {elapsed_precise}/{eta_precise} {pos}/{len} {percent}%",
        )
        .unwrap()
        .progress_chars("#>-"),
    );
    progress_bar.inc(0); // show the bar

    let target_files = params.sync.then(|| Mutex::new(HashSet::new()));

    files.par_iter().try_for_each(|path| -> Result<()> {
        macro_rules! unwrap {
            ($result:expr) => {
                match $result {
                    Ok(val) => val,
                    Err(err) if params.skip_errors => {
                        progress_bar.suspend(|| {
                            log!(false, "Error: {}", err);
                            log!(false, "  when processing file: {}\n", path.display());
                        });
                        progress_bar.inc(1);
                        return Ok(());
                    }
                    Err(err) => return Err(err.into()),
                }
            };
        }

        let extension = params.codec.extension();
        let filename = if params.preserve_structure {
            path.strip_prefix(&params.source).unwrap()
        } else {
            Path::new(path.file_name().unwrap())
        };
        let mut new_path = params.output.join(filename);
        new_path.set_extension(extension);

        let parent = new_path.parent().unwrap();
        if !parent.exists() {
            unwrap!(create_dir_all(parent));
        }

        if let Some(mutex) = &target_files
            && new_path.exists()
        {
            progress_bar.suspend(|| log!(true, "Skipping file: {}", path.display()));
            progress_bar.inc(1);
            mutex.lock().unwrap_or_else(|e| e.into_inner()).insert(new_path);
            return Ok(());
        }

        let mut overwritten_or_filename_altered = false;
        if params.overwrite {
            overwritten_or_filename_altered = new_path.exists();
        } else {
            let mut new_stem = filename.file_stem().unwrap().to_string_lossy().into_owned();
            while new_path.exists() {
                // add a number prefix to the filename stem
                // for example, "thing" renamed to "thing (1)" and "thing (41)" to "thing (42)"
                new_stem = if let Some(left) = new_stem.strip_suffix(')')
                    && let Some((stem, num_str)) = left.rsplit_once(" (")
                    && let Ok(n) = num_str.parse::<usize>()
                {
                    format!("{stem} ({})", n + 1)
                } else {
                    format!("{new_stem} (1)")
                };
                new_path.set_file_name(&new_stem);
                new_path.add_extension(extension);
                overwritten_or_filename_altered = true;
            }
        }

        let temp_path = new_path.with_added_extension("tmp");
        let temp_file = unwrap!(
            OpenOptions::new().read(true).write(true).truncate(true).create(true).open(&temp_path)
        );

        macro_rules! unwrap_clean {
            ($result:expr) => {{
                let result = $result;
                if result.is_err() {
                    let _ = remove_file(&temp_path);
                }
                unwrap!(result)
            }};
        }

        let decoder = unwrap_clean!(params.codec.new_decoder(path));
        let mut encoder =
            unwrap_clean!(params.codec.new_encoder(decoder, temp_file, ImageConfig {
                target_format,
                new_dimensions: params.img_dims,
                quality: params.img_quality.unwrap(),
            }));

        while unwrap_clean!(encoder.write_frame()) {}

        unwrap_clean!(rename(&temp_path, &new_path));
        if overwritten_or_filename_altered {
            let og_filename = path.file_name().unwrap().display();
            let new_filename = new_path.file_name().unwrap().display();
            progress_bar.suspend(|| {
                if params.overwrite {
                    log!(true, "{} saved as and overwrote {}", og_filename, new_filename);
                } else {
                    log!(true, "{} saved as {}", og_filename, new_filename);
                }
            });
        }

        progress_bar.inc(1);
        if let Some(mutex) = &target_files {
            mutex.lock().unwrap_or_else(|e| e.into_inner()).insert(new_path);
        }
        Ok(())
    })?;

    progress_bar.finish();

    if let Some(mutex) = target_files {
        let target_files = mutex.into_inner().unwrap_or_else(|e| e.into_inner());
        let mut unprocessed_targets: Vec<_> = WalkDir::new(&params.output)
            .into_iter()
            .filter_entry(|d| d.file_name().to_str().is_some_and(|s| !s.starts_with('.')))
            .flatten()
            .filter(|d| d.file_type().is_file())
            .map(|d| d.into_path())
            .filter(|p| !target_files.contains(p))
            .collect();
        unprocessed_targets.sort_unstable();
        if unprocessed_targets.is_empty() {
            return Ok(());
        }

        eprintln!("\nThese target files have no corresponding sources:");
        for file in &unprocessed_targets {
            eprintln!("  {}", file.strip_prefix(&params.output).unwrap().display())
        }

        if !params.quiet {
            eprint!("Remove them all? (y/N): ");
            let mut answer = String::new();
            let _ = std::io::stdin().read_line(&mut answer);
            if !(answer == "y\n" || answer == "Y\n") {
                return Ok(());
            }
        }

        for file in unprocessed_targets {
            let _ = remove_file(file);
        }
    }

    Ok(())
}
