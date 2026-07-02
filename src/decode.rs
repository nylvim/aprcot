use std::borrow::Cow;
use std::fs::File;
use std::io::Cursor;
use std::path::Path;

use anyhow::{Result, anyhow, bail, ensure};
use ncmdump::Ncmdump;
use ringbuf::LocalRb;
use ringbuf::storage::Heap;
use ringbuf::traits::{Consumer, Observer, Producer};
use rubato::audioadapter_buffers::direct::{InterleavedSlice, SequentialSlice};
use rubato::{Fft, FixedSync, Resampler};
use symphonia::core::audio::{AudioBuffer, GenericAudioBufferRef};
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::{AudioDecoder, CODEC_ID_NULL_AUDIO};
use symphonia::core::errors::Error::{DecodeError, IoError};
use symphonia::core::formats::FormatReader;
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::StandardTag::{Album, Artist, TrackNumber, TrackTitle};
use symphonia::default::{get_codecs, get_probe};

pub trait Decode {
    fn num_channels(&self) -> usize;
    fn sample_rate(&self) -> usize;
    fn frame_size(&self) -> usize;
    fn metadata(&mut self) -> Metadata;
    fn cover_image(&mut self) -> Option<Vec<u8>>;
    fn next_frame(&mut self, buf: &mut [f32]) -> Result<Option<usize>>;
}

#[derive(Default)]
pub struct Metadata {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub track_number: Option<usize>,
}

pub struct GenericDecoder {
    format_reader: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    resampler: Option<Fft<f32>>,

    num_channels: usize,
    sample_rate: usize,
    frame_size: usize,
    output_interleaved: bool,

    input_buffer: Vec<LocalRb<Heap<f32>>>,
    scratch: Vec<f32>,
    eos_info: Option<usize>,

    // image data from outside of the container
    cover_image: Option<Vec<u8>>,
}

#[derive(Clone, Copy)]
pub struct GenericDecoderConfig {
    pub sample_rate: usize,
    pub frame_size: usize,
    pub output_interleaved: bool,
}

impl GenericDecoderConfig {
    pub const fn opus_default() -> Self {
        use crate::consts::opus::*;
        Self { sample_rate: SAMPLE_RATE, frame_size: FRAME_SIZE, output_interleaved: true }
    }

    pub const fn vorbis_default() -> Self {
        use crate::consts::vorbis::*;
        Self { sample_rate: SAMPLE_RATE, frame_size: FRAME_SIZE, output_interleaved: false }
    }

    pub const fn aac_default() -> Self {
        use crate::consts::aac::*;
        Self { sample_rate: SAMPLE_RATE, frame_size: FRAME_SIZE, output_interleaved: true }
    }
}

impl GenericDecoder {
    pub fn from_reader(
        source: impl MediaSource + 'static,
        config: GenericDecoderConfig,
    ) -> Result<Self> {
        let GenericDecoderConfig { sample_rate, frame_size, output_interleaved } = config;
        let mss = MediaSourceStream::new(Box::new(source), <_>::default());
        let format_reader =
            get_probe().probe(&<_>::default(), mss, <_>::default(), <_>::default())?;

        let track = format_reader
            .tracks()
            .iter()
            .find(|track| {
                matches!(
                    &track.codec_params,
                    Some(CodecParameters::Audio(params))
                    if params.codec != CODEC_ID_NULL_AUDIO
                )
            })
            .ok_or_else(|| anyhow!("no audio track found"))?;
        let track_id = track.id;
        let Some(CodecParameters::Audio(codec_params)) = &track.codec_params else {
            bail!("cannot get codec parameters");
        };

        let decoder = get_codecs().make_audio_decoder(codec_params, &<_>::default())?;

        let source_rate =
            codec_params.sample_rate.ok_or_else(|| anyhow!("unknown sample rate"))? as usize;
        let num_channels =
            codec_params.channels.as_ref().ok_or_else(|| anyhow!("unknown channel count"))?.count();
        ensure!(num_channels <= 2, "wtf kind of music are you listening to");

        let resampler = if source_rate == sample_rate {
            None
        } else {
            Some(Fft::new(
                source_rate,
                sample_rate,
                frame_size,
                1,
                num_channels,
                FixedSync::Output,
            )?)
        };

        let mut input_buffer = Vec::with_capacity(num_channels);
        for _ in 0..num_channels {
            input_buffer.push(LocalRb::new(64 << 10));
        }

        Ok(Self {
            format_reader,
            decoder,
            track_id,
            resampler,
            num_channels,
            sample_rate,
            frame_size,
            output_interleaved,
            input_buffer,
            scratch: Vec::new(),
            eos_info: None,
            cover_image: None,
        })
    }

    pub fn from_file(path: impl AsRef<Path>, config: GenericDecoderConfig) -> Result<Self> {
        if path.as_ref().extension().is_some_and(|s| s == "ncm") {
            Self::from_ncm_file(path, config)
        } else {
            Self::from_reader(File::open(&path)?, config)
        }
    }

    fn from_ncm_file(path: impl AsRef<Path>, config: GenericDecoderConfig) -> Result<Self> {
        let mut dumped = Ncmdump::from_reader(File::open(path)?)?;

        let audio_data = dumped.get_data()?;
        let cover_image = dumped.get_image().ok();

        let mut result = Self::from_reader(Cursor::new(audio_data), config)?;
        result.cover_image = result.cover_image.or(cover_image);

        Ok(result)
    }

    fn fill_input_buffer(&mut self, samples_needed: usize) -> Result<()> {
        while self.input_buffer[0].occupied_len() < samples_needed {
            let packet = match self.format_reader.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => {
                    let num_samples_left = self.input_buffer[0].occupied_len();
                    let num_valid_samples = if let Some(resampler) = &self.resampler {
                        (num_samples_left as f64 * resampler.resample_ratio()).ceil() as usize
                    } else {
                        num_samples_left
                    };
                    self.eos_info = Some(num_valid_samples * self.num_channels);

                    let silence = vec![0.0; samples_needed - num_samples_left];
                    for buffer in &mut self.input_buffer {
                        buffer.push_slice(&silence);
                    }

                    return Ok(());
                }
                Err(err) => return Err(err.into()),
            };

            if packet.track_id != self.track_id {
                continue;
            }

            let decoded = match self.decoder.decode(&packet) {
                Ok(GenericAudioBufferRef::F32(buf)) => Cow::Borrowed(buf),
                Ok(buf) => {
                    let mut new_buf = AudioBuffer::new(buf.spec().clone(), buf.frames());
                    new_buf.resize_uninit(buf.frames());
                    buf.copy_to(&mut new_buf);
                    Cow::Owned(new_buf)
                }
                Err(IoError(_) | DecodeError(_)) => continue,
                Err(err) => return Err(err.into()),
            };

            for (ch, buffer) in self.input_buffer.iter_mut().enumerate() {
                let data = &decoded[ch];
                assert!(
                    buffer.push_slice(data) == data.len(),
                    "input buffer overflow while decoding"
                );
            }
        }

        Ok(())
    }

    fn write_planar(&mut self, buf: &mut [f32], samples_needed: usize) {
        if let Some(resampler) = &mut self.resampler {
            let input_adapter =
                SequentialSlice::new(&self.scratch, self.num_channels, samples_needed).unwrap();
            let mut output_adapter =
                SequentialSlice::new_mut(buf, self.num_channels, self.frame_size).unwrap();
            resampler.process_into_buffer(&input_adapter, &mut output_adapter, None).unwrap();
        } else {
            buf.copy_from_slice(&self.scratch);
        }
    }

    fn write_interleaved(&mut self, buf: &mut [f32], samples_needed: usize) {
        if let Some(resampler) = &mut self.resampler {
            let input_adapter =
                SequentialSlice::new(&self.scratch, self.num_channels, samples_needed).unwrap();
            let mut output_adapter =
                InterleavedSlice::new_mut(buf, self.num_channels, self.frame_size).unwrap();
            resampler.process_into_buffer(&input_adapter, &mut output_adapter, None).unwrap();
        } else {
            for (ch, channel) in self.scratch.chunks_exact(samples_needed).enumerate() {
                for (i, &sample) in channel.iter().enumerate() {
                    buf[i * self.num_channels + ch] = sample;
                }
            }
        }
    }
}

impl Decode for GenericDecoder {
    fn num_channels(&self) -> usize {
        self.num_channels
    }

    fn sample_rate(&self) -> usize {
        self.sample_rate
    }

    fn frame_size(&self) -> usize {
        self.frame_size
    }

    fn metadata(&mut self) -> Metadata {
        let mut metadata = Metadata::default();

        if let Some(revision) = self.format_reader.metadata().current() {
            for tag in &revision.media.tags {
                match &tag.std {
                    Some(TrackTitle(value)) => metadata.title = Some(value.to_string()),
                    Some(Artist(value)) => metadata.artist = Some(value.to_string()),
                    Some(Album(value)) => metadata.album = Some(value.to_string()),
                    Some(TrackNumber(value)) => metadata.track_number = Some(*value as usize),
                    _ => {}
                }
            }
        }

        metadata
    }

    fn cover_image(&mut self) -> Option<Vec<u8>> {
        if self.cover_image.is_some() {
            return self.cover_image.clone();
        }

        self.format_reader
            .metadata()
            .current()
            .and_then(|revision| revision.media.visuals.first())
            .map(|visual| visual.data.to_vec())
    }

    fn next_frame(&mut self, buf: &mut [f32]) -> Result<Option<usize>> {
        ensure!(self.eos_info.is_none(), "EOF reached");

        let samples_needed = if let Some(resampler) = &self.resampler {
            resampler.input_frames_next()
        } else {
            self.frame_size
        };

        self.fill_input_buffer(samples_needed)?;

        self.scratch.resize(self.num_channels * samples_needed, 0.0);
        for (ch, buffer) in self.input_buffer.iter_mut().enumerate() {
            let start = ch * samples_needed;
            let end = start + samples_needed;
            buffer.pop_slice(&mut self.scratch[start..end]);
        }

        if self.output_interleaved {
            self.write_interleaved(buf, samples_needed);
        } else {
            self.write_planar(buf, samples_needed);
        }

        Ok(self.eos_info)
    }
}
