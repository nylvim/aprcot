pub mod aac;
pub mod exhale;
pub mod opus;
pub mod vorbis;

use std::io::{Cursor, Write};

use anyhow::Result;
use image::codecs::avif::AvifEncoder;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType, PngEncoder};
use image::imageops::FilterType::Lanczos3;
use image::{DynamicImage, GenericImageView, ImageReader, load_from_memory};
use webp::Encoder as WebpEncoder;

use self::opus::OpusOggEncoder;
use self::vorbis::VorbisOggEncoder;
use crate::decode::Decode;
use crate::encode::aac::AacM4aEncoder;
use crate::encode::exhale::ExhaleM4aEncoder;

pub trait Encode {
    fn write_frame(&mut self) -> Result<bool>;
}

pub enum GenericEncoder<D, W: Write> {
    Opus(Box<OpusOggEncoder<D, W>>),
    Vorbis(Box<VorbisOggEncoder<D, W>>),
    Aac(Box<AacM4aEncoder<D>>),
    Exhale(Box<ExhaleM4aEncoder<D>>),
}

impl<D: Decode, W: Write> Encode for GenericEncoder<D, W> {
    fn write_frame(&mut self) -> Result<bool> {
        match self {
            Self::Opus(e) => e.write_frame(),
            Self::Vorbis(e) => e.write_frame(),
            Self::Aac(e) => e.write_frame(),
            Self::Exhale(e) => e.write_frame(),
        }
    }
}

pub struct EncoderArgs<D: Decode, W: Write> {
    pub decoder: D,
    pub writer: W,
    pub img_cfg: ImageConfig,
}

struct Image {
    data: Vec<u8>,
    mime_type: &'static str,
    width: u32,
    height: u32,
}

#[derive(Clone, Copy)]
pub enum ImageFormat {
    Copy,
    Png,
    Jpeg,
    Webp,
    Avif,
}

#[derive(Clone, Copy)]
pub struct ImageConfig {
    pub target_format: ImageFormat,
    pub new_dimensions: Option<(u32, u32)>,
    pub quality: f32,
}

impl ImageConfig {
    fn process(&self, data: Vec<u8>) -> Result<Image> {
        use ImageFormat::*;
        match self.target_format {
            Copy => {
                let reader = ImageReader::new(Cursor::new(&data)).with_guessed_format()?;
                let mime_type = reader.format().unwrap().to_mime_type();
                let (width, height) = reader.into_dimensions().unwrap();

                Ok(Image { data, mime_type, width, height })
            }
            Webp => {
                let (image, width, height) = load_and_resize(&data, self.new_dimensions)?;

                let data = WebpEncoder::from_image(&DynamicImage::ImageRgb8(image.to_rgb8()))
                    .unwrap()
                    .encode(self.quality)
                    .to_owned();

                Ok(Image { data, mime_type: "image/webp", width, height })
            }
            Png => {
                let (image, width, height) = load_and_resize(&data, self.new_dimensions)?;

                let mut data = Vec::new();
                image.write_with_encoder(PngEncoder::new_with_quality(
                    &mut data,
                    CompressionType::Default,
                    <_>::default(),
                ))?;

                Ok(Image { data, mime_type: "image/png", width, height })
            }
            Jpeg => {
                let (image, width, height) = load_and_resize(&data, self.new_dimensions)?;

                let mut data = Vec::new();
                image.write_with_encoder(JpegEncoder::new_with_quality(
                    &mut data,
                    self.quality.round() as u8,
                ))?;

                Ok(Image { data, mime_type: "image/jpeg", width, height })
            }
            Avif => {
                let (image, width, height) = load_and_resize(&data, self.new_dimensions)?;

                let mut data = Vec::new();
                image.write_with_encoder(AvifEncoder::new_with_speed_quality(
                    &mut data,
                    5,
                    self.quality.round() as u8,
                ))?;

                Ok(Image { data, mime_type: "image/avif", width, height })
            }
        }
    }
}

fn load_and_resize(
    data: &[u8],
    new_dimensions: Option<(u32, u32)>,
) -> Result<(DynamicImage, u32, u32)> {
    let mut image = load_from_memory(data)?;

    if let Some((nw, nh)) = new_dimensions {
        let (w, h) = image.dimensions();
        if w > nw && h > nh {
            image = image.resize(nw, nh, Lanczos3);
        }
    }
    let (width, height) = image.dimensions();

    Ok((image, width, height))
}
