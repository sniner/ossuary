//! The image extractor: bytes in, what the picture says about itself
//! out — four contracts in one program.
//!
//! Speaks the ossuary extractor protocol (`docs/extractors.md`):
//! `--identify` answers four lines, the `exif`, `xmp`, `iptc` and
//! `raster` contracts, each with its own source and its own receipts.
//! Called with the contract's name as its argument it reads one file's
//! bytes from stdin and answers with findings on stdout, one JSON
//! object per line.
//!
//! `exif` records what EXIF says in EXIF's own terms — tag names
//! kebab-cased, values as the format spells them — and never
//! normalizes; that is the vocabulary's query-time business. `xmp` does
//! the same for the XMP packet, the schema prefix folded into the name,
//! and `iptc` for the IPTC-IIM record. `raster` records what the file's
//! header says about its pixel grid — width, height, bits per channel,
//! alpha, colour model — under `raster:`, as numbers to search by, read
//! without decoding a pixel.
//!
//! Bytes without readable EXIF, XMP or IPTC, or without a header this
//! program reads, are an examination like any other, with nothing
//! found: exit 0, no output. Only failing to read stdin itself is a
//! failure.

mod format;
mod heif;
mod iptc;
mod raster;
mod xmp;

use std::io::Read;
use std::process::ExitCode;

use serde_json::json;

/// The generation of what each contract writes: the number in its
/// source, and so the memory of which files it has seen. Raised by hand
/// when the findings change, when the same bytes would yield more or
/// something different than before, and never for a build, a dependency
/// or a release: a new generation examines every file again, and that
/// is the only reason to have one. Four contracts, four numbers: what
/// `raster` learns to read says nothing about `exif`.
const EXIF_GENERATION: u32 = 1;
const XMP_GENERATION: u32 = 1;
const IPTC_GENERATION: u32 = 1;
const RASTER_GENERATION: u32 = 1;

/// The kinds every reader here opens: the formats whose containers the
/// program looks into for a packet, a record or a header.
const CONTAINERS: [&str; 7] = [
    "image/jpeg",
    "image/tiff",
    "image/png",
    "image/webp",
    "image/heif",
    "image/heic",
    "image/avif",
];

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match arguments
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["--identify"] => {
            println!(
                "{}",
                json!({
                    "ossuary-extractor": 1,
                    "contract": "exif",
                    "source": format!("extractor:image-exif/{EXIF_GENERATION}"),
                    "mimes": CONTAINERS,
                })
            );
            println!(
                "{}",
                json!({
                    "ossuary-extractor": 1,
                    "contract": "xmp",
                    "source": format!("extractor:image-xmp/{XMP_GENERATION}"),
                    "mimes": CONTAINERS,
                })
            );
            println!(
                "{}",
                json!({
                    "ossuary-extractor": 1,
                    "contract": "iptc",
                    "source": format!("extractor:image-iptc/{IPTC_GENERATION}"),
                    "mimes": ["image/jpeg", "image/tiff"],
                })
            );
            println!(
                "{}",
                json!({
                    "ossuary-extractor": 1,
                    "contract": "raster",
                    "source": format!("extractor:image-raster/{RASTER_GENERATION}"),
                    "mimes": CONTAINERS,
                })
            );
            ExitCode::SUCCESS
        }
        ["exif"] => examine(Contract::Exif),
        ["xmp"] => examine(Contract::Xmp),
        ["iptc"] => examine(Contract::Iptc),
        ["raster"] => examine(Contract::Raster),
        _ => {
            eprintln!(
                "ossuary-extract-image: run with --identify, or with one of `exif`, `xmp`, `iptc`, `raster`; a file's bytes on stdin either way"
            );
            ExitCode::FAILURE
        }
    }
}

/// The program's four trades.
#[derive(Clone, Copy)]
enum Contract {
    Exif,
    Xmp,
    Iptc,
    Raster,
}

/// One file's bytes from stdin, the contract's findings on stdout.
fn examine(contract: Contract) -> ExitCode {
    let mut bytes = Vec::new();
    if let Err(error) = std::io::stdin().lock().read_to_end(&mut bytes) {
        eprintln!("ossuary-extract-image: reading stdin: {error}");
        return ExitCode::FAILURE;
    }
    let findings = match contract {
        Contract::Exif => extract(&bytes),
        Contract::Xmp => xmp::read(&bytes),
        Contract::Iptc => iptc::read(&bytes),
        Contract::Raster => raster::read(&bytes)
            .map(|raster| raster.findings())
            .unwrap_or_default(),
    };
    for (attribute, value) in findings {
        println!("{}", json!({ "attribute": attribute, "value": value }));
    }
    ExitCode::SUCCESS
}

/// The tags that say where bytes lie in a TIFF and nothing about the
/// picture: the strips' and tiles' offsets and byte counts, the
/// thumbnail's, and the pointers to the other directories. A scan's
/// `StripOffsets` is one number per strip, thousands of them, and no
/// question is ever asked of it.
const LAYOUT: [exif::Tag; 10] = [
    exif::Tag::StripOffsets,
    exif::Tag::StripByteCounts,
    exif::Tag::RowsPerStrip,
    exif::Tag::TileOffsets,
    exif::Tag::TileByteCounts,
    exif::Tag::JPEGInterchangeFormat,
    exif::Tag::JPEGInterchangeFormatLength,
    exif::Tag::ExifIFDPointer,
    exif::Tag::GPSInfoIFDPointer,
    exif::Tag::InteropIFDPointer,
];

/// Every EXIF field of the primary image, in EXIF's own words: the tag
/// name kebab-cased under `exif:`, the value as the format stores it —
/// text as text (the date's own colons included), numbers as numbers,
/// rationals as `numerator/denominator`, one value bare and several as a
/// list. Opaque byte blobs — `MakerNote` and kin — are not worth quoting,
/// the file's own layout is no finding, and bytes without readable EXIF
/// yield nothing at all: that, too, is an answer.
fn extract(bytes: &[u8]) -> Vec<(String, serde_json::Value)> {
    let mut cursor = std::io::Cursor::new(bytes);
    let Ok(data) = exif::Reader::new().read_from_container(&mut cursor) else {
        return Vec::new();
    };
    let mut findings = Vec::new();
    for field in data.fields() {
        if field.ifd_num != exif::In::PRIMARY {
            // The thumbnail's fields describe the thumbnail.
            continue;
        }
        if field.tag.description().is_none() {
            // A tag the EXIF crate cannot name has no attribute to live
            // under; naming it by number would freeze a guess.
            continue;
        }
        if LAYOUT.contains(&field.tag) {
            continue;
        }
        if let Some(value) = render(&field.value) {
            findings.push((format!("exif:{}", kebab(&field.tag.to_string())), value));
        }
    }
    findings
}

/// A field's value in the format's own spelling — deliberately not the
/// EXIF crate's display form, which quotes strings and tidies dates.
fn render(value: &exif::Value) -> Option<serde_json::Value> {
    use exif::Value;
    let rational = |numerator: i64, denominator: i64| json!(format!("{numerator}/{denominator}"));
    let values: Vec<serde_json::Value> = match value {
        Value::Ascii(strings) => strings
            .iter()
            .map(|bytes| json!(String::from_utf8_lossy(bytes)))
            .collect(),
        Value::Byte(numbers) => numbers.iter().map(|&n| json!(n)).collect(),
        Value::SByte(numbers) => numbers.iter().map(|&n| json!(n)).collect(),
        Value::Short(numbers) => numbers.iter().map(|&n| json!(n)).collect(),
        Value::SShort(numbers) => numbers.iter().map(|&n| json!(n)).collect(),
        Value::Long(numbers) => numbers.iter().map(|&n| json!(n)).collect(),
        Value::SLong(numbers) => numbers.iter().map(|&n| json!(n)).collect(),
        Value::Rational(ratios) => ratios
            .iter()
            .map(|r| rational(r.num.into(), r.denom.into()))
            .collect(),
        Value::SRational(ratios) => ratios
            .iter()
            .map(|r| rational(r.num.into(), r.denom.into()))
            .collect(),
        Value::Float(numbers) => numbers
            .iter()
            .map(|&n| serde_json::Number::from_f64(n.into()).map(serde_json::Value::Number))
            .collect::<Option<_>>()?,
        Value::Double(numbers) => numbers
            .iter()
            .map(|&n| serde_json::Number::from_f64(n).map(serde_json::Value::Number))
            .collect::<Option<_>>()?,
        Value::Undefined(..) | Value::Unknown(..) => return None,
    };
    match values.len() {
        0 => None,
        1 => values.into_iter().next(),
        _ => Some(serde_json::Value::Array(values)),
    }
}

/// `DateTimeOriginal` → `date-time-original`, `ISOSpeed` → `iso-speed`:
/// a word starts at an uppercase letter after a lowercase one, and at the
/// last uppercase letter of a run when lowercase follows it. The `xmp`
/// contract draws its word boundaries the same way.
pub(crate) fn kebab(name: &str) -> String {
    let characters: Vec<char> = name.chars().collect();
    let mut result = String::with_capacity(name.len() + 4);
    for (position, &character) in characters.iter().enumerate() {
        if character.is_ascii_uppercase() && position > 0 {
            let after_lower = characters[position - 1].is_ascii_lowercase()
                || characters[position - 1].is_ascii_digit();
            let before_lower = characters
                .get(position + 1)
                .is_some_and(char::is_ascii_lowercase);
            if after_lower || before_lower {
                result.push('-');
            }
        }
        result.push(character.to_ascii_lowercase());
    }
    result
}

#[cfg(test)]
mod tests {
    use exif::experimental::Writer;
    use exif::{Field, In, Tag, Value};

    use super::*;

    #[test]
    fn tag_names_become_attribute_names() {
        assert_eq!(kebab("DateTimeOriginal"), "date-time-original");
        assert_eq!(kebab("FNumber"), "f-number");
        assert_eq!(kebab("ISOSpeed"), "iso-speed");
        assert_eq!(kebab("GPSLatitudeRef"), "gps-latitude-ref");
        assert_eq!(kebab("PhotographicSensitivity"), "photographic-sensitivity");
    }

    /// A minimal TIFF stream carrying the fields, as a camera would.
    fn sample() -> Vec<u8> {
        let date = Field {
            tag: Tag::DateTimeOriginal,
            ifd_num: In::PRIMARY,
            value: Value::Ascii(vec![b"2019:07:14 11:02:41".to_vec()]),
        };
        let make = Field {
            tag: Tag::Make,
            ifd_num: In::PRIMARY,
            value: Value::Ascii(vec![b"Example Cameras Inc.".to_vec()]),
        };
        let mut writer = Writer::new();
        writer.push_field(&date);
        writer.push_field(&make);
        let mut buffer = std::io::Cursor::new(Vec::new());
        writer.write(&mut buffer, false).unwrap();
        buffer.into_inner()
    }

    #[test]
    fn what_exif_says_comes_out_verbatim() {
        let findings = extract(&sample());
        assert!(
            findings.contains(&(
                "exif:date-time-original".to_string(),
                json!("2019:07:14 11:02:41")
            )),
            "EXIF's own spelling, colons and all — normalizing is query-time business; got {findings:?}"
        );
        assert!(
            findings.contains(&("exif:make".to_string(), json!("Example Cameras Inc."))),
            "text as text, no display quoting; got {findings:?}"
        );
    }

    #[test]
    fn a_rational_is_spelled_as_the_fraction_it_is() {
        let aperture = Field {
            tag: Tag::FNumber,
            ifd_num: In::PRIMARY,
            value: Value::Rational(vec![exif::Rational { num: 28, denom: 10 }]),
        };
        let mut writer = Writer::new();
        writer.push_field(&aperture);
        let mut buffer = std::io::Cursor::new(Vec::new());
        writer.write(&mut buffer, false).unwrap();

        let findings = extract(&buffer.into_inner());
        assert!(
            findings.contains(&("exif:f-number".to_string(), json!("28/10"))),
            "the stored fraction, not a prettied decimal; got {findings:?}"
        );
    }

    #[test]
    fn a_tiffs_layout_is_no_finding() {
        let mut out = std::io::Cursor::new(Vec::new());
        let mut encoder = tiff::encoder::TiffEncoder::new(&mut out).unwrap();
        let mut image = encoder
            .new_image::<tiff::encoder::colortype::Gray8>(2, 2)
            .unwrap();
        image
            .encoder()
            .write_tag(tiff::tags::Tag::Software, "Example Scanner 1.0")
            .unwrap();
        image.write_data(&[0u8; 4]).unwrap();

        let findings = extract(&out.into_inner());
        assert!(
            findings.contains(&("exif:software".to_string(), json!("Example Scanner 1.0"))),
            "got {findings:?}"
        );
        assert!(
            findings.contains(&("exif:image-width".to_string(), json!(2))),
            "what the picture is stays; got {findings:?}"
        );
        assert!(
            findings.iter().all(|(attribute, _)| {
                !matches!(
                    attribute.as_str(),
                    "exif:strip-offsets" | "exif:strip-byte-counts" | "exif:rows-per-strip"
                )
            }),
            "where the bytes lie is not; got {findings:?}"
        );
    }

    #[test]
    fn bytes_without_exif_are_an_empty_answer_not_an_error() {
        assert_eq!(extract(b"plain words"), Vec::new());
        assert_eq!(extract(&[]), Vec::new());
    }
}
