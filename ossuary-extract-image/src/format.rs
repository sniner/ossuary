//! What the first bytes announce, and the one reader every contract
//! shares: a JPEG's headers, read once and asked for the frame header by
//! `raster`, for the APP segments by `xmp` and `iptc`.

use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::bytestream::ZCursor;
use zune_jpeg::zune_core::options::DecoderOptions;

/// The container a file's signature names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Jpeg,
    Png,
    Tiff,
    WebP,
    /// The ISO base media family: HEIF, HEIC, AVIF, told apart inside.
    Heif,
}

/// The kind these bytes announce, by their signature alone.
pub fn sniff(bytes: &[u8]) -> Option<Kind> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(Kind::Png)
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(Kind::Jpeg)
    } else if bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*") {
        Some(Kind::Tiff)
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP".as_slice()) {
        Some(Kind::WebP)
    } else if bytes.get(4..8) == Some(b"ftyp".as_slice()) {
        Some(Kind::Heif)
    } else {
        None
    }
}

/// A JPEG's decoder with its headers read and no pixel decoded; `None`
/// when the bytes are no JPEG this reader accepts.
pub fn jpeg_headers(bytes: &[u8]) -> Option<JpegDecoder<ZCursor<&[u8]>>> {
    let options = DecoderOptions::default()
        .set_strict_mode(false)
        .set_max_width(usize::MAX)
        .set_max_height(usize::MAX);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(bytes), options);
    decoder.decode_headers().ok()?;
    Some(decoder)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_name_their_kind() {
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n...."), Some(Kind::Png));
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), Some(Kind::Jpeg));
        assert_eq!(sniff(b"II*\0\x08\0\0\0"), Some(Kind::Tiff));
        assert_eq!(sniff(b"MM\0*\0\0\0\x08"), Some(Kind::Tiff));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some(Kind::WebP));
        assert_eq!(sniff(b"\0\0\0\x18ftypheic"), Some(Kind::Heif));
        assert_eq!(sniff(b"plain words"), None);
        assert_eq!(sniff(&[]), None);
    }
}
