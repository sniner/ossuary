//! The `iptc` contract: the IPTC-IIM record — the press vocabulary of
//! caption, keywords, credit and copyright that predates XMP and still
//! rides along in JPEGs and TIFFs — in IPTC's own words: each dataset's
//! name kebab-cased under `iptc:`, the value as the record spells it.
//! `Caption/Abstract` becomes `iptc:caption-abstract`, `By-line`
//! `iptc:by-line`, `Date Created` stays `"20190714"`.
//!
//! A dataset said several times — keywords are — stands as a list. The
//! text is read as UTF-8 where the record declares it (dataset 1:90) or
//! where the bytes happen to be valid UTF-8, and as Latin-1 otherwise,
//! which is what writers of the undeclared kind used. Datasets this
//! program cannot name, and the binary ones, are left out: numbering
//! them would freeze a guess.

use serde_json::{Value, json};

use crate::format::{self, Kind};

/// Record 2, the application record: the datasets by number and the
/// names the IIM specification gives them.
const DATASETS: &[(u8, &str)] = &[
    (3, "Object Type Reference"),
    (4, "Object Attribute Reference"),
    (5, "Object Name"),
    (7, "Edit Status"),
    (8, "Editorial Update"),
    (10, "Urgency"),
    (12, "Subject Reference"),
    (15, "Category"),
    (20, "Supplemental Category"),
    (22, "Fixture Identifier"),
    (25, "Keywords"),
    (26, "Content Location Code"),
    (27, "Content Location Name"),
    (30, "Release Date"),
    (35, "Release Time"),
    (37, "Expiration Date"),
    (38, "Expiration Time"),
    (40, "Special Instructions"),
    (42, "Action Advised"),
    (45, "Reference Service"),
    (47, "Reference Date"),
    (50, "Reference Number"),
    (55, "Date Created"),
    (60, "Time Created"),
    (62, "Digital Creation Date"),
    (63, "Digital Creation Time"),
    (65, "Originating Program"),
    (70, "Program Version"),
    (75, "Object Cycle"),
    (80, "By-line"),
    (85, "By-line Title"),
    (90, "City"),
    (92, "Sub-location"),
    (95, "Province/State"),
    (100, "Country/Primary Location Code"),
    (101, "Country/Primary Location Name"),
    (103, "Original Transmission Reference"),
    (105, "Headline"),
    (110, "Credit"),
    (115, "Source"),
    (116, "Copyright Notice"),
    (118, "Contact"),
    (120, "Caption/Abstract"),
    (121, "Local Caption"),
    (122, "Writer/Editor"),
    (130, "Image Type"),
    (131, "Image Orientation"),
    (135, "Language Identifier"),
];

/// The findings of the IPTC record these bytes carry.
pub fn read(bytes: &[u8]) -> Vec<(String, Value)> {
    block(bytes)
        .map(|block| datasets(&block))
        .unwrap_or_default()
}

/// The record, where the container keeps it: a JPEG's APP13 segment,
/// inside a Photoshop image resource block; a TIFF's tag 33723, bare,
/// or its tag 34377, the same resource block again.
fn block(bytes: &[u8]) -> Option<Vec<u8>> {
    match format::sniff(bytes)? {
        Kind::Jpeg => format::jpeg_headers(bytes)?.info()?.iptc_data,
        Kind::Tiff => {
            let mut decoder = tiff::decoder::Decoder::new(std::io::Cursor::new(bytes)).ok()?;
            [33723, 34377].into_iter().find_map(|tag| {
                let value = decoder.find_tag(tiff::tags::Tag::Unknown(tag)).ok()??;
                match value {
                    tiff::decoder::ifd::Value::Ascii(text) => Some(text.into_bytes()),
                    // Old writers typed the record LONG; the bytes are the same.
                    tiff::decoder::ifd::Value::List(ref items)
                        if items
                            .iter()
                            .all(|item| matches!(item, tiff::decoder::ifd::Value::Unsigned(_))) =>
                    {
                        value
                            .into_u32_vec()
                            .ok()
                            .map(|words| words.iter().flat_map(|word| word.to_be_bytes()).collect())
                    }
                    other => other.into_u8_vec().ok(),
                }
            })
        }
        Kind::Png | Kind::WebP | Kind::Heif => None,
    }
}

/// The IIM datasets of a record, or of the Photoshop resource block
/// (`8BIM`) that wraps one as resource 0x0404.
fn datasets(block: &[u8]) -> Vec<(String, Value)> {
    let record = if block.starts_with(b"8BIM") {
        match resource(block, 0x0404) {
            Some(record) => record,
            None => return Vec::new(),
        }
    } else {
        block
    };
    let entries = entries(record);
    let utf8 = entries
        .iter()
        .any(|&(number, dataset, data)| number == 1 && dataset == 90 && data == b"\x1b%G");
    let mut findings: Vec<(String, Vec<String>)> = Vec::new();
    for (number, dataset, data) in entries {
        if number != 2 {
            continue;
        }
        let Some((_, name)) = DATASETS.iter().find(|(known, _)| *known == dataset) else {
            continue;
        };
        let text = decode(data, utf8);
        let attribute = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect::<String>();
        if let Some((_, values)) = findings.iter_mut().find(|(known, _)| *known == attribute) {
            values.push(text);
        } else {
            findings.push((attribute, vec![text]));
        }
    }
    findings
        .into_iter()
        .map(|(attribute, mut values)| {
            let value = if values.len() == 1 {
                json!(values.remove(0))
            } else {
                json!(values)
            };
            (format!("iptc:{attribute}"), value)
        })
        .collect()
}

/// One resource of a Photoshop image resource block, by its id: the
/// block is a run of `8BIM`, id, a Pascal name padded to even length,
/// a size, and the data padded to even length.
fn resource(block: &[u8], wanted: u16) -> Option<&[u8]> {
    let mut rest = block;
    while rest.len() >= 12 && rest.starts_with(b"8BIM") {
        let id = u16::from_be_bytes([rest[4], rest[5]]);
        let name_length = rest[6] as usize;
        let name_end = 7 + name_length;
        let name_end = if name_end % 2 == 0 {
            name_end
        } else {
            name_end + 1
        };
        let size_end = name_end + 4;
        let size_bytes = rest.get(name_end..size_end)?;
        let size = u32::from_be_bytes([size_bytes[0], size_bytes[1], size_bytes[2], size_bytes[3]])
            as usize;
        let data = rest.get(size_end..size_end + size)?;
        if id == wanted {
            return Some(data);
        }
        let next = size_end + size;
        rest = &rest[if next % 2 == 0 { next } else { next + 1 }.min(rest.len())..];
    }
    None
}

/// The datasets of an IIM record in order: `(record, dataset, data)`.
/// Each is a marker `0x1C`, the record and dataset numbers, a length —
/// two bytes, or with the high bit set the length of the length — and
/// the data.
fn entries(record: &[u8]) -> Vec<(u8, u8, &[u8])> {
    let mut entries = Vec::new();
    let mut at = 0;
    while let Some(&[0x1C, number, dataset, high, low]) = record.get(at..at + 5) {
        let (length, data_at) = if high & 0x80 == 0 {
            (usize::from(u16::from_be_bytes([high, low])), at + 5)
        } else {
            let count = usize::from(u16::from_be_bytes([high & 0x7F, low]));
            if count > 4 {
                break;
            }
            let Some(bytes) = record.get(at + 5..at + 5 + count) else {
                break;
            };
            let length = bytes
                .iter()
                .fold(0usize, |acc, &byte| (acc << 8) | usize::from(byte));
            (length, at + 5 + count)
        };
        let Some(data) = record.get(data_at..data_at + length) else {
            break;
        };
        entries.push((number, dataset, data));
        at = data_at + length;
    }
    entries
}

/// The text of a dataset: UTF-8 where declared or where it holds, Latin-1
/// otherwise; trailing NULs, which some writers pad with, are no text.
fn decode(data: &[u8], utf8: bool) -> String {
    let data = data.strip_suffix(b"\0").unwrap_or(data);
    if utf8 || std::str::from_utf8(data).is_ok() {
        String::from_utf8_lossy(data).into_owned()
    } else {
        data.iter().map(|&byte| char::from(byte)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dataset(number: u8, set: u8, data: &[u8]) -> Vec<u8> {
        let mut out = vec![0x1C, number, set];
        out.extend_from_slice(&(u16::try_from(data.len()).unwrap()).to_be_bytes());
        out.extend_from_slice(data);
        out
    }

    fn record(sets: &[(u8, &[u8])]) -> Vec<u8> {
        let mut out = dataset(2, 0, &[0, 4]);
        for &(set, data) in sets {
            out.extend(dataset(2, set, data));
        }
        out
    }

    fn photoshop_block(resources: &[(u16, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for &(id, data) in resources {
            out.extend_from_slice(b"8BIM");
            out.extend_from_slice(&id.to_be_bytes());
            out.extend_from_slice(&[0, 0]); // empty name, padded
            out.extend_from_slice(&(u32::try_from(data.len()).unwrap()).to_be_bytes());
            out.extend_from_slice(data);
            if data.len() % 2 == 1 {
                out.push(0);
            }
        }
        out
    }

    #[test]
    fn datasets_come_out_by_name_and_repeat_as_a_list() {
        let record = record(&[
            (5, b"Alps at dawn"),
            (25, b"alps"),
            (25, b"summer"),
            (55, b"20190714"),
            (120, b"The Alps, seen from the south."),
            (116, b"(c) Someone"),
        ]);
        assert_eq!(
            datasets(&record),
            vec![
                ("iptc:object-name".to_string(), json!("Alps at dawn")),
                ("iptc:keywords".to_string(), json!(["alps", "summer"])),
                ("iptc:date-created".to_string(), json!("20190714")),
                (
                    "iptc:caption-abstract".to_string(),
                    json!("The Alps, seen from the south.")
                ),
                ("iptc:copyright-notice".to_string(), json!("(c) Someone")),
            ],
            "the record version, binary, is not a finding"
        );
    }

    #[test]
    fn latin1_stays_readable_and_declared_utf8_is_trusted() {
        let latin1 = record(&[(90, b"Z\xfcrich")]);
        assert_eq!(
            datasets(&latin1),
            vec![("iptc:city".to_string(), json!("Zürich"))]
        );
        let mut declared = dataset(1, 90, b"\x1b%G");
        declared.extend(record(&[(90, "Zürich".as_bytes())]));
        assert_eq!(
            datasets(&declared),
            vec![("iptc:city".to_string(), json!("Zürich"))]
        );
    }

    #[test]
    fn the_photoshop_block_is_unwrapped_to_its_iptc_resource() {
        let block = photoshop_block(&[
            (0x03ED, &[0, 0, 0, 72, 0, 1]),
            (0x0404, &record(&[(105, b"Headline")])),
        ]);
        assert_eq!(
            datasets(&block),
            vec![("iptc:headline".to_string(), json!("Headline"))]
        );
        assert_eq!(datasets(&photoshop_block(&[(0x03ED, &[1])])), Vec::new());
    }

    #[test]
    fn an_extended_length_is_read_and_a_torn_record_stops_short() {
        let mut extended = vec![0x1C, 2, 105, 0x80, 2, 0, 3];
        extended.extend_from_slice(b"abc");
        assert_eq!(
            datasets(&extended),
            vec![("iptc:headline".to_string(), json!("abc"))]
        );
        let mut torn = record(&[(105, b"whole")]);
        torn.extend_from_slice(&[0x1C, 2, 25, 0, 9, b'h', b'a']);
        assert_eq!(
            datasets(&torn),
            vec![("iptc:headline".to_string(), json!("whole"))]
        );
    }

    #[test]
    fn a_jpeg_carries_its_record_in_app13() {
        let mut segment = b"Photoshop 3.0\0".to_vec();
        segment.extend(photoshop_block(&[(0x0404, &record(&[(25, b"tagged")]))]));
        let mut out = Vec::new();
        let mut encoder = jpeg_encoder::Encoder::new(&mut out, 90);
        encoder.add_app_segment(13, &segment).unwrap();
        encoder
            .encode(&[0; 12], 2, 2, jpeg_encoder::ColorType::Rgb)
            .unwrap();
        assert_eq!(
            read(&out),
            vec![("iptc:keywords".to_string(), json!("tagged"))]
        );
    }

    #[test]
    fn a_tiff_carries_its_record_in_tag_33723() {
        let mut out = std::io::Cursor::new(Vec::new());
        let mut encoder = tiff::encoder::TiffEncoder::new(&mut out).unwrap();
        let mut image = encoder
            .new_image::<tiff::encoder::colortype::Gray8>(1, 1)
            .unwrap();
        image
            .encoder()
            .write_tag(
                tiff::tags::Tag::Unknown(33723),
                record(&[(25, b"tagged")]).as_slice(),
            )
            .unwrap();
        image.write_data(&[0u8]).unwrap();
        assert_eq!(
            read(&out.into_inner()),
            vec![("iptc:keywords".to_string(), json!("tagged"))]
        );
    }

    #[test]
    fn bytes_without_a_record_are_an_empty_answer() {
        assert_eq!(read(b"plain words"), Vec::new());
        assert_eq!(read(&[]), Vec::new());
        assert_eq!(datasets(b"\x1C"), Vec::new());
    }
}
