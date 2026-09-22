//! The `raster` contract: what a file's header says about its pixel
//! grid — width and height in pixels, bits per channel, whether there
//! is an alpha channel, and the colour model — read from the header
//! alone, no pixel decoded.
//!
//! Each format's own reader answers, so the answer is the file's word,
//! not a decoder's: a four-bit palette PNG says four bits and `indexed`,
//! not the eight-bit RGB it would be expanded to. The colour model is
//! the one the pixels are stored in, with one reading applied: a JPEG's
//! YCbCr is `rgb`, since that is what it encodes, in another coordinate
//! system. A HEIF's grid is what its `meta` box declares for the primary
//! picture, the codestream unopened. Bytes that are no image this
//! contract reads, or whose header cannot be read, are an examination
//! with nothing found.

use std::io::Cursor;

use serde_json::{Value, json};
use zune_jpeg::zune_core::colorspace::ColorSpace;

use crate::format::{self, Kind};

/// The pixel grid as the header describes it.
#[derive(Debug, PartialEq, Eq)]
pub struct Raster {
    pub width: u32,
    pub height: u32,
    /// Bits per channel, as stored.
    pub depth: u8,
    pub alpha: bool,
    /// `gray`, `rgb`, `cmyk` or `indexed`; `None` where the format names
    /// no colour model, as a multiband TIFF does not.
    pub color: Option<&'static str>,
}

impl Raster {
    /// The findings, one attribute each, under `raster:`.
    pub fn findings(&self) -> Vec<(String, Value)> {
        let mut findings = vec![
            ("raster:width".to_string(), json!(self.width)),
            ("raster:height".to_string(), json!(self.height)),
            ("raster:depth".to_string(), json!(self.depth)),
            ("raster:alpha".to_string(), json!(self.alpha)),
        ];
        if let Some(color) = self.color {
            findings.push(("raster:color".to_string(), json!(color)));
        }
        findings
    }
}

/// The raster of these bytes, by the format their first bytes announce.
pub fn read(bytes: &[u8]) -> Option<Raster> {
    match format::sniff(bytes)? {
        Kind::Png => png(bytes),
        Kind::Jpeg => jpeg(bytes),
        Kind::Tiff => tiff(bytes),
        Kind::WebP => webp(bytes),
        Kind::Heif => crate::heif::parse(bytes)?.raster(),
    }
}

/// IHDR, PLTE and tRNS: everything before the first IDAT.
fn png(bytes: &[u8]) -> Option<Raster> {
    let reader = png::Decoder::new(Cursor::new(bytes)).read_info().ok()?;
    let info = reader.info();
    let (color, channel_alpha) = match info.color_type {
        png::ColorType::Grayscale => ("gray", false),
        png::ColorType::GrayscaleAlpha => ("gray", true),
        png::ColorType::Rgb => ("rgb", false),
        png::ColorType::Rgba => ("rgb", true),
        png::ColorType::Indexed => ("indexed", false),
    };
    Some(Raster {
        width: info.width,
        height: info.height,
        depth: info.bit_depth as u8,
        // A tRNS chunk gives a palette, or one colour, its transparency.
        alpha: channel_alpha || info.trns.is_some(),
        color: Some(color),
    })
}

/// The frame header. JPEG as read here is eight bits per channel; a
/// twelve-bit file is refused by the reader, and so has nothing found.
fn jpeg(bytes: &[u8]) -> Option<Raster> {
    let decoder = format::jpeg_headers(bytes)?;
    let info = decoder.info()?;
    let color = match decoder.input_colorspace()? {
        ColorSpace::Luma | ColorSpace::LumaA => "gray",
        ColorSpace::RGB
        | ColorSpace::RGBA
        | ColorSpace::BGR
        | ColorSpace::BGRA
        | ColorSpace::YCbCr => "rgb",
        ColorSpace::CMYK | ColorSpace::YCCK => "cmyk",
        _ => return None,
    };
    Some(Raster {
        width: info.width.into(),
        height: info.height.into(),
        depth: 8,
        alpha: false,
        color: Some(color),
    })
}

/// The first image file directory.
fn tiff(bytes: &[u8]) -> Option<Raster> {
    use tiff::ColorType;
    let mut decoder = tiff::decoder::Decoder::new(Cursor::new(bytes)).ok()?;
    let (width, height) = decoder.dimensions().ok()?;
    let (depth, alpha, color) = match decoder.colortype().ok()? {
        ColorType::Gray(depth) => (depth, false, Some("gray")),
        ColorType::GrayA(depth) => (depth, true, Some("gray")),
        ColorType::RGB(depth) | ColorType::YCbCr(depth) => (depth, false, Some("rgb")),
        ColorType::RGBA(depth) => (depth, true, Some("rgb")),
        ColorType::Palette(depth) => (depth, false, Some("indexed")),
        ColorType::CMYK(depth) => (depth, false, Some("cmyk")),
        ColorType::CMYKA(depth) => (depth, true, Some("cmyk")),
        ColorType::Multiband { bit_depth, .. } => (bit_depth, false, None),
        _ => return None,
    };
    Some(Raster {
        width,
        height,
        depth,
        alpha,
        color,
    })
}

/// The RIFF chunk headers; a WebP is eight-bit RGB, with or without
/// alpha.
fn webp(bytes: &[u8]) -> Option<Raster> {
    let decoder = image_webp::WebPDecoder::new(Cursor::new(bytes)).ok()?;
    let (width, height) = decoder.dimensions();
    Some(Raster {
        width,
        height,
        depth: 8,
        alpha: decoder.has_alpha(),
        color: Some("rgb"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_bytes(
        width: u32,
        height: u32,
        color: png::ColorType,
        depth: png::BitDepth,
        palette: Option<&[u8]>,
        trns: Option<&[u8]>,
        data: &[u8],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(color);
        encoder.set_depth(depth);
        if let Some(palette) = palette {
            encoder.set_palette(palette.to_vec());
        }
        if let Some(trns) = trns {
            encoder.set_trns(trns.to_vec());
        }
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(data).unwrap();
        drop(writer);
        out
    }

    #[test]
    fn a_png_says_its_channels_and_depth() {
        let rgba = png_bytes(
            2,
            1,
            png::ColorType::Rgba,
            png::BitDepth::Eight,
            None,
            None,
            &[0; 8],
        );
        assert_eq!(
            read(&rgba),
            Some(Raster {
                width: 2,
                height: 1,
                depth: 8,
                alpha: true,
                color: Some("rgb"),
            })
        );

        let gray16 = png_bytes(
            1,
            3,
            png::ColorType::Grayscale,
            png::BitDepth::Sixteen,
            None,
            None,
            &[0; 6],
        );
        assert_eq!(
            read(&gray16),
            Some(Raster {
                width: 1,
                height: 3,
                depth: 16,
                alpha: false,
                color: Some("gray"),
            })
        );
    }

    #[test]
    fn a_palette_png_stays_indexed_at_its_own_depth() {
        // Two four-bit indices in one byte, a palette of two colours, and
        // a tRNS chunk that makes the second one transparent.
        let indexed = png_bytes(
            2,
            1,
            png::ColorType::Indexed,
            png::BitDepth::Four,
            Some(&[0, 0, 0, 255, 255, 255]),
            Some(&[255, 0]),
            &[0x01],
        );
        assert_eq!(
            read(&indexed),
            Some(Raster {
                width: 2,
                height: 1,
                depth: 4,
                alpha: true,
                color: Some("indexed"),
            }),
            "the file's four bits and its palette, not a decoder's eight-bit expansion"
        );
    }

    fn jpeg_bytes(width: u16, height: u16, color: jpeg_encoder::ColorType, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        jpeg_encoder::Encoder::new(&mut out, 90)
            .encode(data, width, height, color)
            .unwrap();
        out
    }

    #[test]
    fn a_jpeg_names_its_colour_model() {
        let photo = jpeg_bytes(3, 2, jpeg_encoder::ColorType::Rgb, &[0; 18]);
        assert_eq!(
            read(&photo),
            Some(Raster {
                width: 3,
                height: 2,
                depth: 8,
                alpha: false,
                color: Some("rgb"),
            }),
            "YCbCr is rgb in another coordinate system"
        );

        let scan = jpeg_bytes(2, 2, jpeg_encoder::ColorType::Luma, &[0; 4]);
        assert_eq!(read(&scan).unwrap().color, Some("gray"));

        let print = jpeg_bytes(2, 2, jpeg_encoder::ColorType::Cmyk, &[0; 16]);
        assert_eq!(read(&print).unwrap().color, Some("cmyk"));
    }

    #[test]
    fn a_tiff_says_its_depth_and_alpha() {
        let mut out = Cursor::new(Vec::new());
        tiff::encoder::TiffEncoder::new(&mut out)
            .unwrap()
            .write_image::<tiff::encoder::colortype::Gray16>(2, 2, &[0u16; 4])
            .unwrap();
        assert_eq!(
            read(&out.into_inner()),
            Some(Raster {
                width: 2,
                height: 2,
                depth: 16,
                alpha: false,
                color: Some("gray"),
            })
        );

        let mut out = Cursor::new(Vec::new());
        tiff::encoder::TiffEncoder::new(&mut out)
            .unwrap()
            .write_image::<tiff::encoder::colortype::RGBA8>(1, 1, &[0u8; 4])
            .unwrap();
        let rgba = read(&out.into_inner()).unwrap();
        assert!(rgba.alpha);
        assert_eq!(rgba.color, Some("rgb"));
    }

    #[test]
    fn a_webp_says_whether_it_has_alpha() {
        let mut out = Vec::new();
        image_webp::WebPEncoder::new(&mut out)
            .encode(&[0; 8], 2, 1, image_webp::ColorType::Rgba8)
            .unwrap();
        assert_eq!(
            read(&out),
            Some(Raster {
                width: 2,
                height: 1,
                depth: 8,
                alpha: true,
                color: Some("rgb"),
            })
        );

        let mut out = Vec::new();
        image_webp::WebPEncoder::new(&mut out)
            .encode(&[0; 6], 2, 1, image_webp::ColorType::Rgb8)
            .unwrap();
        assert!(!read(&out).unwrap().alpha);
    }

    #[test]
    fn bytes_that_are_no_raster_are_an_empty_answer() {
        assert_eq!(read(b"plain words"), None);
        assert_eq!(read(&[]), None);
        // A PNG signature with nothing behind it: a header that cannot be read.
        assert_eq!(read(b"\x89PNG\r\n\x1a\n"), None);
    }

    #[test]
    fn findings_carry_the_raster_namespace() {
        let raster = Raster {
            width: 4,
            height: 3,
            depth: 8,
            alpha: false,
            color: Some("rgb"),
        };
        assert_eq!(
            raster.findings(),
            vec![
                ("raster:width".to_string(), json!(4)),
                ("raster:height".to_string(), json!(3)),
                ("raster:depth".to_string(), json!(8)),
                ("raster:alpha".to_string(), json!(false)),
                ("raster:color".to_string(), json!("rgb")),
            ]
        );
        let unnamed = Raster {
            color: None,
            ..raster
        };
        assert_eq!(
            unnamed.findings().len(),
            4,
            "no colour model, no colour claim"
        );
    }
}
