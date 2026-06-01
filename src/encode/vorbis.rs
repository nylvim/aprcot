use std::io::Write;

use anyhow::Result;
use base64::prelude::*;
use vorbis_rs::{VorbisBitrateManagementStrategy, VorbisEncoder, VorbisEncoderBuilder};

use super::{Encode, Image, ImageConfig};
use crate::consts::vorbis::SERIAL;
use crate::decode::{Decode, Metadata};

pub struct VorbisOggEncoder<D, W: Write> {
    decoder: D,
    encoder: VorbisEncoder<W>,
    input_buffer: Vec<f32>,
}

impl<D: Decode, W: Write> VorbisOggEncoder<D, W> {
    pub fn new(
        mut decoder: D,
        writer: W,
        img_cfg: ImageConfig,
        vbr: bool,
        bitrate: u32,
        quality: f32,
    ) -> Result<Self> {
        let num_channels = decoder.num_channels();
        let sample_rate = decoder.sample_rate();
        let frame_size = decoder.frame_size();

        let bms = if vbr {
            VorbisBitrateManagementStrategy::Vbr { target_bitrate: bitrate.try_into()? }
        } else {
            VorbisBitrateManagementStrategy::QualityVbr { target_quality: quality }
        };

        let image = decoder.cover_image();
        let image = image.map(|data| img_cfg.process(data)).transpose()?;
        let comment_tags = build_vorbis_comments(decoder.metadata(), image.as_ref());

        let encoder = VorbisEncoderBuilder::new_with_serial(
            (sample_rate as u32).try_into()?,
            (num_channels as u8).try_into()?,
            writer,
            SERIAL,
        )
        .bitrate_management_strategy(bms)
        .comment_tags(comment_tags)?
        .build()?;

        Ok(Self { decoder, encoder, input_buffer: vec![0.0; num_channels * frame_size] })
    }
}

impl<D: Decode, W: Write> Encode for VorbisOggEncoder<D, W> {
    fn write_frame(&mut self) -> Result<bool> {
        let eos_info = self.decoder.next_frame(&mut self.input_buffer)?;
        let channels: Vec<_> = self.input_buffer.chunks_exact(self.decoder.frame_size()).collect();
        self.encoder.encode_audio_block(&channels)?;
        Ok(eos_info.is_none())
    }
}

fn build_vorbis_comments(metadata: Metadata, image: Option<&Image>) -> Vec<(&'static str, String)> {
    let mut comments = Vec::new();

    if let Some(title) = metadata.title {
        comments.push(("TITLE", title));
    }
    if let Some(artist) = metadata.artist {
        comments.push(("ARTIST", artist));
    }
    if let Some(album) = metadata.album {
        comments.push(("ALBUM", album));
    }
    if let Some(track_number) = metadata.track_number {
        comments.push(("TRACKNUMBER", track_number.to_string()));
    }

    if let Some(Image { data, mime_type, width, height }) = image {
        let mut buffer = Vec::new();
        buffer.extend(3_u32.to_be_bytes()); // 3 for "Front Cover"
        buffer.extend((mime_type.len() as u32).to_be_bytes());
        buffer.extend(mime_type.as_bytes());
        buffer.extend(0_u32.to_be_bytes()); // description length
        buffer.extend(width.to_be_bytes());
        buffer.extend(height.to_be_bytes());
        buffer.extend(24_u32.to_be_bytes()); // color depth
        buffer.extend(0_u32.to_be_bytes()); // 0 for non-indexed pictures (non-GIF)
        buffer.extend((data.len() as u32).to_be_bytes());
        buffer.extend(&**data);

        let encoded = BASE64_STANDARD.encode(buffer);
        comments.push(("METADATA_BLOCK_PICTURE", encoded));
    }

    comments
}
