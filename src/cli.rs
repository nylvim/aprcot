use std::error::Error;
use std::fs::File;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail, ensure};
use clap::{Parser, Subcommand};

use crate::decode::{Decode, GenericDecoder, GenericDecoderConfig};
use crate::encode::aac::AacM4aEncoder;
use crate::encode::exhale::ExhaleM4aEncoder;
use crate::encode::opus::OpusOggEncoder;
use crate::encode::vorbis::VorbisOggEncoder;
use crate::encode::{EncoderArgs, GenericEncoder, ImageConfig};

#[cfg(windows)]
const USAGE_STRING: &str = "aprcot.exe <CODEC> [OPTIONS] <SOURCE> <OUTPUT>";

#[cfg(not(windows))]
const USAGE_STRING: &str = "aprcot <CODEC> [OPTIONS] <SOURCE> <OUTPUT>";

fn parse_int<T>(
    name: &'static str,
    range: RangeInclusive<i64>,
) -> impl Fn(&str) -> Result<T> + Clone
where
    T: TryFrom<i64, Error: Error + Send + Sync + 'static>,
{
    move |s| {
        let val = s.parse()?;
        ensure!(
            range.contains(&val),
            "{name} must be in range {} to {}",
            range.start(),
            range.end()
        );
        Ok(val.try_into()?)
    }
}

fn parse_float(
    name: &'static str,
    range: RangeInclusive<f32>,
) -> impl Fn(&str) -> Result<f32> + Clone {
    move |s| {
        let val = s.parse()?;
        ensure!(
            range.contains(&val),
            "{name} must be in range {} to {}",
            range.start(),
            range.end()
        );
        Ok(val)
    }
}

fn parse_dims(s: &str) -> Result<(u32, u32)> {
    if let Some((w, h)) = s.split_once('x')
        && let (Ok(w), Ok(h)) = (w.parse(), h.parse())
    {
        Ok((w, h))
    } else {
        bail!("dimensions must be in the format of \"WxH\"");
    }
}

#[derive(Parser)]
#[command(
    override_usage = USAGE_STRING,
    args_conflicts_with_subcommands = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub codec: Codec,

    /// Directory of files to be processed, scanned recursively
    #[arg(global = true)]
    pub source: PathBuf,
    /// Directory to store the outputs
    #[arg(global = true)]
    pub output: PathBuf,

    /// Number of CPUs to use, default is all
    #[arg(short, long, global = true)]
    pub jobs: Option<usize>,
    /// Preserve directory structure
    #[arg(short = 'z', long, global = true)]
    pub preserve_structure: bool,
    /// Overwrite existing file, otherwise add suffix to the new file's name
    #[arg(short, long, global = true)]
    pub overwrite: bool,
    /// Perform an incremental update, with an option to remove outdated targets
    #[arg(short, long, global = true)]
    pub sync: bool,
    /// Skip files that failed to be processed
    #[arg(short = 'i', long, global = true)]
    pub skip_errors: bool,
    /// Print more detailed logs
    #[arg(short, long, global = true)]
    pub verbose: bool,
    /// Suppress all log messages
    #[arg(short = 'x', long, global = true, conflicts_with = "verbose")]
    pub quiet: bool,

    /// Image format
    #[arg(short, long, global = true,
        value_parser = ["copy", "png", "jpeg", "webp", "avif"], default_value = "copy")]
    pub format: Option<String>,
    /// Max width and height of cover images, in the format `WxH`
    #[arg(short = 'd', long, global = true, value_parser = parse_dims)]
    pub img_dims: Option<(u32, u32)>,
    /// Image encoding quality, range from 0.0 to 100.0
    #[arg(short = 'p', long, global = true,
        value_parser = parse_float("image quality", 0.0..=100.0), default_value = "90")]
    pub img_quality: Option<f32>,
}

#[derive(Subcommand)]
#[command(
    subcommand_help_heading = "Codecs",
    subcommand_value_name = "CODEC",
    disable_help_subcommand = true
)]
pub enum Codec {
    /// Recommended, best quality
    #[command(short_flag = 'O', long_flag = "opus")]
    Opus {
        /// Bitrate in kbps
        #[arg(short, long, value_parser = parse_float("bitrate", 6.0..=256.0), default_value = "108")]
        bitrate: Option<f32>,
        /// Encoding complexity, range from 1 to 10
        #[arg(short, long, value_parser = parse_int::<u8>("complexity", 1..=10), default_value = "10")]
        complexity: Option<u8>,
    },
    /// Also recommended, fastest encoding and good quality
    #[command(short_flag = 'A', long_flag = "aac")]
    Aac {
        /// Use CBR instead of VBR
        #[arg(long)]
        cbr: bool,
        /// CBR bitrate in kbps
        #[arg(short, long,
            value_parser = parse_float("bitrate", 8.0..=576.0), default_value = "128",)]
        bitrate: Option<f32>,
        /// VBR quality level, range from 1 to 5
        #[arg(short, long, value_parser = parse_int::<u8>("quality", 1..=5), default_value = "3",
            conflicts_with_all = ["cbr", "bitrate"])]
        quality: Option<u8>,
        /// AAC profile. HE-AAC and HE-AACv2 are only recommended for very low bitrates
        #[arg(long, value_parser = ["lc", "he", "hev2"], default_value = "lc")]
        profile: Option<String>,
    },
    /// Not recommended, slower encoding and slightly worse quality than AAC
    #[command(short_flag = 'V', long_flag = "vorbis")]
    Vorbis {
        /// Use bitrate-based VBR instead of quality-based QVBR
        #[arg(long)]
        vbr: bool,
        /// VBR bitrate in kbps
        #[arg(short, long, default_value = "160")]
        bitrate: Option<f32>,
        /// QVBR quality level, range from -1.0 to 10.0
        #[arg(short, long, value_parser = parse_float("quality", -1.0..=10.0),
            allow_negative_numbers = true, default_value = "5",
            conflicts_with_all = ["vbr", "bitrate"])]
        quality: Option<f32>,
    },
    /// Not recommended, very slow encoding, just for experiment
    #[command(short_flag = 'X', long_flag = "xhe-aac")]
    XheAac {
        /// Enable eSBR, only recommended for very low bitrates
        #[arg(long)]
        sbr: bool,
        /// VBR quality level, range from 0 to 9
        #[arg(short, long, value_parser = parse_int::<u8>("quality", 1..=9), default_value = "3")]
        quality: Option<u8>,
    },
}

impl Codec {
    pub fn extension(&self) -> &str {
        match self {
            Self::Opus { .. } => crate::consts::opus::EXTENSION,
            Self::Vorbis { .. } => crate::consts::vorbis::EXTENSION,
            Self::Aac { .. } => crate::consts::aac::EXTENSION,
            Self::XheAac { .. } => crate::consts::aac::EXTENSION,
        }
    }

    pub fn new_decoder(&self, path: impl AsRef<Path>) -> Result<GenericDecoder> {
        match self {
            Self::Opus { .. } => {
                GenericDecoder::from_file(path, GenericDecoderConfig::opus_default())
            }
            Self::Vorbis { .. } => {
                GenericDecoder::from_file(path, GenericDecoderConfig::vorbis_default())
            }
            Self::Aac { profile, .. } => {
                let config = if profile.as_deref().unwrap() == "lc" {
                    GenericDecoderConfig::aac_default()
                } else {
                    let mut cfg = GenericDecoderConfig::aac_default();
                    cfg.frame_size *= 2;
                    cfg
                };
                GenericDecoder::from_file(path, config)
            }
            Self::XheAac { sbr, .. } => {
                let config = if !sbr {
                    GenericDecoderConfig::aac_default()
                } else {
                    let mut cfg = GenericDecoderConfig::aac_default();
                    cfg.frame_size *= 2;
                    cfg
                };
                GenericDecoder::from_file(path, config)
            }
        }
    }

    pub fn new_encoder<D: Decode>(
        &self,
        decoder: D,
        writer: File,
        img_cfg: ImageConfig,
    ) -> Result<GenericEncoder<D, File>> {
        let encoder_args = EncoderArgs { decoder, writer, img_cfg };
        match self {
            Self::Opus { bitrate, complexity } => {
                let bitrate = (bitrate.unwrap() * 1000.0).round() as i32;
                let complexity = complexity.unwrap() as i32;
                let encoder = OpusOggEncoder::new(encoder_args, bitrate, complexity)?;
                Ok(GenericEncoder::Opus(Box::new(encoder)))
            }
            Self::Vorbis { vbr, bitrate, quality } => {
                let bitrate = (bitrate.unwrap() * 1000.0).round() as u32;
                let quality = quality.unwrap() / 10.0;
                let encoder = VorbisOggEncoder::new(encoder_args, *vbr, bitrate, quality)?;
                Ok(GenericEncoder::Vorbis(Box::new(encoder)))
            }
            Self::Aac { cbr, bitrate, quality, profile } => {
                let bitrate = (bitrate.unwrap() * 1000.0).round() as u32;
                let quality = quality.unwrap();
                let profile = profile.as_deref().unwrap();
                let encoder = AacM4aEncoder::new(encoder_args, *cbr, bitrate, quality, profile)?;
                Ok(GenericEncoder::Aac(Box::new(encoder)))
            }
            Self::XheAac { sbr, quality } => {
                let quality = quality.unwrap();
                let encoder = ExhaleM4aEncoder::new(encoder_args, quality, *sbr)?;
                Ok(GenericEncoder::Exhale(Box::new(encoder)))
            }
        }
    }
}
