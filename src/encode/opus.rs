use std::io::Write;

use anyhow::Result;
use ogg::{PacketWriteEndInfo, PacketWriter};
use opus::{Application, Bitrate, Channels, Encoder as OpusEncoder};

use super::vorbis::build_vorbis_comments;
use super::{Encode, EncoderArgs, Image};
use crate::consts::opus::{SERIAL, VENDOR_STRING};
use crate::decode::{Decode, Metadata};

pub struct OpusOggEncoder<D, W: Write> {
    decoder: D,
    encoder: OpusEncoder,
    packet_writer: PacketWriter<'static, W>,
    input_buffer: Vec<f32>,
    granule_position: u64,
}

impl<D: Decode, W: Write> OpusOggEncoder<D, W> {
    pub fn new(
        EncoderArgs { mut decoder, writer, img_cfg }: EncoderArgs<D, W>,
        bitrate: i32,
        complexity: i32,
    ) -> Result<Self> {
        let num_channels = decoder.num_channels();
        let sample_rate = decoder.sample_rate();
        let frame_size = decoder.frame_size();

        let channels = match num_channels {
            1 => Channels::Mono,
            2 => Channels::Stereo,
            _ => unreachable!(),
        };

        let mut encoder = OpusEncoder::new(sample_rate as u32, channels, Application::Audio)?;
        encoder.set_bitrate(Bitrate::Bits(bitrate))?;
        encoder.set_complexity(complexity)?;

        let mut packet_writer = PacketWriter::new(writer);

        let image = decoder.cover_image();
        let image = image.map(|data| img_cfg.process(data)).transpose()?;
        let id_header = build_ogg_id_header(num_channels, sample_rate, encoder.get_lookahead()?);
        let comment_header = build_ogg_comment_header(decoder.metadata(), image.as_ref());
        packet_writer.write_packet(id_header, SERIAL, PacketWriteEndInfo::EndPage, 0)?;
        packet_writer.write_packet(comment_header, SERIAL, PacketWriteEndInfo::EndPage, 0)?;

        Ok(Self {
            decoder,
            encoder,
            packet_writer,
            input_buffer: vec![0.0; num_channels * frame_size],
            granule_position: 0,
        })
    }
}
impl<D: Decode, W: Write> Encode for OpusOggEncoder<D, W> {
    fn write_frame(&mut self) -> Result<bool> {
        let eos_info = self.decoder.next_frame(&mut self.input_buffer)?;
        let payload = self.encoder.encode_vec_float(&self.input_buffer, 4 << 10)?;

        let packet_info = if let Some(num_samples_left) = eos_info {
            self.granule_position += num_samples_left as u64;
            PacketWriteEndInfo::EndStream
        } else {
            self.granule_position += self.decoder.frame_size() as u64;
            PacketWriteEndInfo::NormalPacket
        };

        self.packet_writer.write_packet(payload, SERIAL, packet_info, self.granule_position)?;
        Ok(eos_info.is_none())
    }
}

fn build_ogg_id_header(num_channels: usize, sample_rate: usize, pre_skip: i32) -> Vec<u8> {
    let mut header = Vec::with_capacity(19);
    header.extend(b"OpusHead");
    header.push(1); // version
    header.push(num_channels as u8);
    header.extend((pre_skip as u16).to_le_bytes());
    header.extend((sample_rate as u32).to_le_bytes());
    header.extend(0_i16.to_le_bytes()); // gain
    header.push(0); // mapping family
    header
}

fn build_ogg_comment_header(metadata: Metadata, image: Option<&Image>) -> Vec<u8> {
    let mut header = Vec::new();

    header.extend(b"OpusTags");
    header.extend((VENDOR_STRING.len() as u32).to_le_bytes());
    header.extend(VENDOR_STRING);

    let comments: Vec<_> = build_vorbis_comments(metadata, image)
        .into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    header.extend((comments.len() as u32).to_le_bytes());
    for comment in comments {
        header.extend((comment.len() as u32).to_le_bytes());
        header.extend(comment.as_bytes());
    }

    header
}
