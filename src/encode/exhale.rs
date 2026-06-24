use std::fs::File;
use std::iter::zip;

use anyhow::Result;
use exhale::{Channels, Encoder, EncoderConfig};
use mp4::{AacConfig, MediaConfig, Mp4Config, Mp4Sample, Mp4Writer, TrackConfig, TrackType};

use super::aac::write_mp4_tags;
use super::{Encode, Image, ImageConfig};
use crate::decode::{Decode, Metadata};

pub struct ExhaleM4aEncoder<D, W = File> {
    decoder: D,
    encoder: Encoder,
    mp4_writer: Option<Mp4Writer<W>>,
    decode_buffer: Vec<f32>,
    input_buffer: Vec<i32>,
    current_time: u64,

    metadata: Option<Metadata>,
    cover_image: Option<Option<Image>>,
}

impl<D: Decode> ExhaleM4aEncoder<D> {
    pub fn new(
        mut decoder: D,
        writer: File,
        img_cfg: ImageConfig,
        vbr_level: u8,
        enable_sbr: bool,
    ) -> Result<Self> {
        let num_channels = decoder.num_channels();
        let sample_rate = decoder.sample_rate() as u32;
        let frame_size = decoder.frame_size();

        let channels = match num_channels {
            1 => Channels::Mono,
            2 => Channels::Stereo,
            _ => unreachable!(),
        };

        let config =
            EncoderConfig { sample_rate, channels, enable_sbr, vbr_level, ..Default::default() };
        let encoder = Encoder::new(config)?;
        let input_buffer = vec![0; encoder.frame_size() * num_channels];

        let mp4_config = Mp4Config {
            major_brand: "M4A ".parse().unwrap(),
            minor_version: 0,
            compatible_brands: vec![
                "M4A ".parse().unwrap(),
                "mp42".parse().unwrap(),
                "isom".parse().unwrap(),
                "iso2".parse().unwrap(),
            ],
            timescale: 1000,
        };

        let track_config = TrackConfig {
            track_type: TrackType::Audio,
            timescale: sample_rate,
            language: "und".into(),
            media_conf: MediaConfig::AacConfig(AacConfig {
                asc_override: Some(encoder.asc_data().to_vec()),
                ..Default::default()
            }),
        };

        let mut mp4_writer = Mp4Writer::write_start(writer, &mp4_config)?;
        mp4_writer.add_track(&track_config)?;

        let metadata = decoder.metadata();
        let image = decoder.cover_image();
        let image = image.map(|data| img_cfg.process(data)).transpose()?;

        Ok(Self {
            decoder,
            encoder,
            mp4_writer: Some(mp4_writer),
            decode_buffer: vec![0.0; num_channels * frame_size],
            input_buffer,
            current_time: 0,
            metadata: Some(metadata),
            cover_image: Some(image),
        })
    }
}

impl<D: Decode> Encode for ExhaleM4aEncoder<D> {
    fn write_frame(&mut self) -> Result<bool> {
        const I24_MIN: i32 = -8388608;
        const I24_MAX: i32 = 8388607;

        let eos_info = self.decoder.next_frame(&mut self.decode_buffer)?;
        for (&f, i) in zip(&self.decode_buffer, &mut self.input_buffer) {
            *i = ((f * I24_MIN as f32).round() as i32).clamp(I24_MIN, I24_MAX);
        }
        let result = self.encoder.encode_frame(&self.input_buffer)?.to_owned();

        let mp4_sample = Mp4Sample {
            start_time: self.current_time,
            duration: self.decoder.frame_size() as u32,
            rendering_offset: 0,
            is_sync: true,
            bytes: result.into(),
        };
        self.mp4_writer.as_mut().unwrap().write_sample(1, &mp4_sample)?;
        self.current_time += self.decoder.frame_size() as u64;

        if eos_info.is_some() {
            let mut mp4_writer = self.mp4_writer.take().unwrap();
            mp4_writer.write_end()?;
            let mut writer = mp4_writer.into_writer();
            write_mp4_tags(
                &mut writer,
                self.metadata.take().unwrap(),
                self.cover_image.take().unwrap(),
            )?;
        }

        Ok(eos_info.is_none())
    }
}
