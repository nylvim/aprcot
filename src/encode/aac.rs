use std::fs::File;
use std::iter::zip;

use anyhow::{Result, anyhow, bail};
use fdk_aac::enc::{
    AudioObjectType as AacAOT, BitRate, ChannelMode, EncodeInfo, Encoder as FdkAacEncoder,
    EncoderParams, Transport,
};
use mp4::{
    AacConfig, AudioObjectType as Mp4AOT, ChannelConfig, MediaConfig, Mp4Config, Mp4Sample,
    Mp4Writer, SampleFreqIndex, TrackConfig, TrackType,
};
use mp4ameta::{Img, StorageFile, Tag};

use super::{Encode, EncoderArgs, Image};
use crate::decode::{Decode, Metadata};

pub struct AacM4aEncoder<D, W = File> {
    decoder: D,
    encoder: FdkAacEncoder,
    mp4_writer: Option<Mp4Writer<W>>,
    decode_buffer: Vec<f32>,
    input_buffer: Vec<i16>,
    current_time: u64,

    metadata: Option<Metadata>,
    cover_image: Option<Option<Image>>,
}

impl<D: Decode> AacM4aEncoder<D> {
    pub fn new(
        EncoderArgs { mut decoder, writer, img_cfg }: EncoderArgs<D, File>,
        cbr: bool,
        bitrate: u32,
        quality: u8,
        profile: &str,
    ) -> Result<Self> {
        let num_channels = decoder.num_channels();
        let sample_rate = decoder.sample_rate();
        let frame_size = decoder.frame_size();

        let channels = match num_channels {
            1 => ChannelMode::Mono,
            2 => ChannelMode::Stereo,
            _ => unreachable!(),
        };

        let bit_rate = if cbr {
            BitRate::Cbr(bitrate)
        } else {
            match quality {
                1 => BitRate::VbrVeryLow,
                2 => BitRate::VbrLow,
                3 => BitRate::VbrMedium,
                4 => BitRate::VbrHigh,
                5 => BitRate::VbrVeryHigh,
                _ => unreachable!(),
            }
        };

        let (aac_aot, mp4_aot) = match profile {
            "lc" => (AacAOT::Mpeg4LowComplexity, Mp4AOT::AacLowComplexity),
            "he" => (AacAOT::Mpeg4HeAac, Mp4AOT::SpectralBandReplication),
            "hev2" => (AacAOT::Mpeg4HeAacV2, Mp4AOT::ParametricStereo),
            _ => unreachable!(),
        };

        let encoder = FdkAacEncoder::new(EncoderParams {
            bit_rate,
            sample_rate: sample_rate as u32,
            transport: Transport::Raw,
            channels,
            audio_object_type: aac_aot,
        })
        .map_err(|err| anyhow!("{err}"))?;

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

        let actual_bitrate = if cbr { bitrate } else { 0 };
        let freq_index = match sample_rate {
            44100 => SampleFreqIndex::Freq44100,
            48000 => SampleFreqIndex::Freq48000,
            96000 => SampleFreqIndex::Freq96000,
            _ => unimplemented!(),
        };
        let chan_conf = match num_channels {
            1 => ChannelConfig::Mono,
            2 => ChannelConfig::Stereo,
            _ => unreachable!(),
        };
        let media_conf = MediaConfig::AacConfig(AacConfig {
            bitrate: actual_bitrate,
            profile: mp4_aot,
            freq_index,
            chan_conf,
            asc_override: None,
        });
        let track_config = TrackConfig {
            track_type: TrackType::Audio,
            timescale: sample_rate as u32,
            language: "und".into(),
            media_conf,
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
            input_buffer: vec![0; num_channels * frame_size],
            current_time: 0,
            metadata: Some(metadata),
            cover_image: Some(image),
        })
    }
}

impl<D: Decode> Encode for AacM4aEncoder<D> {
    fn write_frame(&mut self) -> Result<bool> {
        let eos_info = self.decoder.next_frame(&mut self.decode_buffer)?;
        let num_samples = eos_info.unwrap_or(self.decode_buffer.len());

        for (&f, i) in zip(&self.decode_buffer, &mut self.input_buffer) {
            *i = (f * i16::MAX as f32).round() as i16;
        }

        let mut output_buffer = vec![0; 2 << 10];
        let EncodeInfo { output_size, .. } = self
            .encoder
            .encode(&self.input_buffer[..num_samples], &mut output_buffer)
            .map_err(|err| anyhow!("{err}"))?;
        output_buffer.truncate(output_size);

        let mp4_sample = Mp4Sample {
            start_time: self.current_time,
            duration: self.decoder.frame_size() as u32,
            rendering_offset: 0,
            is_sync: true,
            bytes: output_buffer.into(),
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

pub(super) fn write_mp4_tags(
    writer: &mut impl StorageFile,
    metadata: Metadata,
    image: Option<Image>,
) -> Result<()> {
    let mut tag = Tag::default();

    if let Some(title) = metadata.title {
        tag.set_title(title);
    }
    if let Some(artist) = metadata.artist {
        tag.set_artist(artist);
    }
    if let Some(album) = metadata.album {
        tag.set_album(album);
    }
    if let Some(track_number) = metadata.track_number {
        tag.set_track_number(track_number as u16);
    }
    if let Some(image) = image {
        match image.mime_type {
            "image/jpeg" => tag.set_artwork(Img::jpeg(image.data)),
            "image/png" => tag.set_artwork(Img::png(image.data)),
            _ => bail!("MP4 format only accepts JPEG or PNG"),
        }
    }

    tag.write_to(writer)?;
    Ok(())
}
