//! Where the picture was taken, as one value under `geo:position`,
//! computed from the GPS fields of its EXIF data. The `exif` contract
//! records it beside the fields themselves, which stay unchanged, so
//! the claim's source names the format the position was read from.
//!
//! EXIF gives latitude and longitude as three fractions each (degrees,
//! minutes, seconds) and the hemisphere in a field of its own. The
//! position is an object with `latitude` and `longitude` in decimal
//! degrees, negative to the south and west, and `altitude` in metres,
//! negative below the reference.
//!
//! No position is recorded when one of the fields it needs is missing or
//! cannot be read, when the receiver marked the measurement void, or
//! when the map datum is not WGS 84: converting between datums would
//! take more than the file states. A position whose altitude cannot be
//! read is recorded without it. Each such case is reported on stderr.
//! EXIF without GPS coordinates yields nothing and no report.

use exif::{Exif, Field, In, Tag};
use serde_json::{Map, Value, json};

/// The position the EXIF data states, and the reports about GPS fields
/// that could not be used.
pub fn read(data: &Exif) -> (Vec<(String, Value)>, Vec<String>) {
    let field = |tag| data.get_field(tag, In::PRIMARY);
    if field(Tag::GPSLatitude).is_none() && field(Tag::GPSLongitude).is_none() {
        return (Vec::new(), Vec::new());
    }
    match position(data) {
        Ok((position, remark)) => (
            vec![("geo:position".to_string(), position)],
            remark.into_iter().collect(),
        ),
        Err(reason) => (
            Vec::new(),
            vec![format!("geo:position not recorded, {reason}")],
        ),
    }
}

/// The position as an object, with a report when the altitude had to
/// be left out; or why there is none.
fn position(data: &Exif) -> Result<(Value, Option<String>), String> {
    let field = |tag| data.get_field(tag, In::PRIMARY);
    if let Some(status) = field(Tag::GPSStatus)
        && text(status).is_some_and(|text| text.trim().eq_ignore_ascii_case("V"))
    {
        return Err("exif:gps-status is V, the measurement is void".to_string());
    }
    if let Some(datum) = field(Tag::GPSMapDatum)
        && !is_wgs84(datum)
    {
        return Err(format!(
            "exif:gps-map-datum is not WGS 84: {}",
            shown(datum)
        ));
    }
    let latitude = coordinate(
        data,
        Tag::GPSLatitude,
        Tag::GPSLatitudeRef,
        ("N", "S"),
        90.0,
    )?;
    let longitude = coordinate(
        data,
        Tag::GPSLongitude,
        Tag::GPSLongitudeRef,
        ("E", "W"),
        180.0,
    )?;
    let mut position = Map::new();
    position.insert("latitude".to_string(), json!(latitude));
    position.insert("longitude".to_string(), json!(longitude));
    let remark = match altitude(data) {
        Ok(Some(altitude)) => {
            position.insert("altitude".to_string(), json!(altitude));
            None
        }
        Ok(None) => None,
        Err(reason) => Some(format!("geo:position recorded without altitude, {reason}")),
    };
    Ok((Value::Object(position), remark))
}

/// One coordinate in decimal degrees: degrees, minutes and seconds from
/// `tag`, the sign from the hemisphere letter in `reference`.
fn coordinate(
    data: &Exif,
    tag: Tag,
    reference: Tag,
    (positive, negative): (&str, &str),
    limit: f64,
) -> Result<f64, String> {
    let name = attribute(tag);
    let Some(field) = data.get_field(tag, In::PRIMARY) else {
        return Err(format!("no {name}"));
    };
    let exif::Value::Rational(parts) = &field.value else {
        return Err(format!("{name} is not three fractions: {}", shown(field)));
    };
    let [degrees, minutes, seconds] = parts.as_slice() else {
        return Err(format!("{name} is not three fractions: {}", shown(field)));
    };
    if parts.iter().any(|part| part.denom == 0) {
        return Err(format!(
            "{name} has a fraction with denominator 0: {}",
            shown(field)
        ));
    }
    let value = degrees.to_f64() + minutes.to_f64() / 60.0 + seconds.to_f64() / 3600.0;
    if value > limit {
        return Err(format!("{name} is out of range: {}", shown(field)));
    }
    let reference_name = attribute(reference);
    let Some(hemisphere) = data.get_field(reference, In::PRIMARY) else {
        return Err(format!("no {reference_name}"));
    };
    let letter = text(hemisphere).map(|text| text.trim().to_ascii_uppercase());
    match letter.as_deref() {
        Some(letter) if letter == positive => Ok(value),
        // A coordinate of 0 has no hemisphere, and -0.0 would be
        // written as such.
        Some(letter) if letter == negative => Ok(if value == 0.0 { 0.0 } else { -value }),
        _ => Err(format!(
            "{reference_name} is not {positive} or {negative}: {}",
            shown(hemisphere)
        )),
    }
}

/// The altitude in metres, negative below the reference; `None` when
/// the file gives none.
///
/// Without `GPSAltitudeRef`, the altitude is above the reference: 0 is
/// the field's default in the EXIF specification. Exif 3.0 added the
/// values 2 and 3 for a second kind of reference, with the same signs.
fn altitude(data: &Exif) -> Result<Option<f64>, String> {
    let Some(field) = data.get_field(Tag::GPSAltitude, In::PRIMARY) else {
        return Ok(None);
    };
    let name = attribute(Tag::GPSAltitude);
    let exif::Value::Rational(parts) = &field.value else {
        return Err(format!("{name} is not a fraction: {}", shown(field)));
    };
    let [height] = parts.as_slice() else {
        return Err(format!("{name} is not a fraction: {}", shown(field)));
    };
    if height.denom == 0 {
        return Err(format!(
            "{name} has a fraction with denominator 0: {}",
            shown(field)
        ));
    }
    let height = height.to_f64();
    let Some(reference) = data.get_field(Tag::GPSAltitudeRef, In::PRIMARY) else {
        return Ok(Some(height));
    };
    match &reference.value {
        exif::Value::Byte(values) if matches!(values.as_slice(), [0 | 2]) => Ok(Some(height)),
        exif::Value::Byte(values) if matches!(values.as_slice(), [1 | 3]) => {
            Ok(Some(if height == 0.0 { 0.0 } else { -height }))
        }
        _ => Err(format!(
            "{} is not 0, 1, 2 or 3: {}",
            attribute(Tag::GPSAltitudeRef),
            shown(reference)
        )),
    }
}

/// Whether the map datum names WGS 84, in any of the spellings in use
/// (`WGS-84`, `WGS84`, `WGS 84`, `WGS-1984`). An empty datum names none,
/// and GPS receivers measure in WGS 84.
fn is_wgs84(datum: &Field) -> bool {
    let Some(text) = text(datum) else {
        return false;
    };
    let letters: String = text
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_uppercase())
        .collect();
    matches!(letters.as_str(), "" | "WGS84" | "WGS1984")
}

/// The text of an ASCII field.
fn text(field: &Field) -> Option<String> {
    match &field.value {
        exif::Value::Ascii(strings) => Some(
            strings
                .iter()
                .map(|bytes| String::from_utf8_lossy(bytes))
                .collect::<Vec<_>>()
                .join(" "),
        ),
        _ => None,
    }
}

/// A field's value for a report, the way the `exif` contract records
/// it, without the quotes of JSON: `48/1 8/1 0/0`, `TOKYO`.
fn shown(field: &Field) -> String {
    let plain = |value: &Value| {
        value
            .as_str()
            .map_or_else(|| value.to_string(), str::to_string)
    };
    match crate::render(&field.value) {
        Some(Value::Array(items)) => items.iter().map(plain).collect::<Vec<_>>().join(" "),
        Some(value) => plain(&value),
        None => "a value without a type".to_string(),
    }
}

/// The attribute the `exif` contract records the field under.
fn attribute(tag: Tag) -> String {
    format!("exif:{}", crate::kebab(&tag.to_string()))
}

#[cfg(test)]
mod tests {
    use exif::experimental::Writer;
    use exif::{Rational, Value as Exif};

    use super::*;

    fn field(tag: Tag, value: Exif) -> Field {
        Field {
            tag,
            ifd_num: In::PRIMARY,
            value,
        }
    }

    fn ascii(text: &str) -> Exif {
        Exif::Ascii(vec![text.as_bytes().to_vec()])
    }

    fn fractions(parts: &[(u32, u32)]) -> Exif {
        Exif::Rational(
            parts
                .iter()
                .map(|&(num, denom)| Rational { num, denom })
                .collect(),
        )
    }

    /// The position of the fields, written into a TIFF stream as a
    /// camera writes them and read back.
    fn examined(fields: &[Field]) -> (Vec<(String, Value)>, Vec<String>) {
        let mut writer = Writer::new();
        for field in fields {
            writer.push_field(field);
        }
        let mut buffer = std::io::Cursor::new(Vec::new());
        writer.write(&mut buffer, false).unwrap();
        let mut bytes = std::io::Cursor::new(buffer.into_inner());
        read(&exif::Reader::new().read_from_container(&mut bytes).unwrap())
    }

    /// Munich, 519.5 m above sea level: 48° 8' 13.75" N, 11° 34' 34.04" E.
    fn munich() -> Vec<Field> {
        vec![
            field(Tag::GPSLatitudeRef, ascii("N")),
            field(Tag::GPSLatitude, fractions(&[(48, 1), (8, 1), (1375, 100)])),
            field(Tag::GPSLongitudeRef, ascii("E")),
            field(
                Tag::GPSLongitude,
                fractions(&[(11, 1), (34, 1), (3404, 100)]),
            ),
            field(Tag::GPSAltitudeRef, Exif::Byte(vec![0])),
            field(Tag::GPSAltitude, fractions(&[(5195, 10)])),
        ]
    }

    fn with(mut fields: Vec<Field>, replaced: Field) -> Vec<Field> {
        fields.retain(|field| field.tag != replaced.tag);
        fields.push(replaced);
        fields
    }

    fn without(mut fields: Vec<Field>, tag: Tag) -> Vec<Field> {
        fields.retain(|field| field.tag != tag);
        fields
    }

    fn close(value: &Value, part: &str, expected: f64) {
        let found = value[part].as_f64().unwrap();
        assert!(
            (found - expected).abs() < 1e-9,
            "{part}: {found} != {expected}"
        );
    }

    #[test]
    fn degrees_minutes_seconds_become_decimal_degrees() {
        let (findings, remarks) = examined(&munich());
        assert_eq!(remarks, Vec::<String>::new());
        let [(attribute, position)] = findings.as_slice() else {
            panic!("one position, got {findings:?}");
        };
        assert_eq!(attribute, "geo:position");
        close(position, "latitude", 48.0 + 8.0 / 60.0 + 13.75 / 3600.0);
        close(position, "longitude", 11.0 + 34.0 / 60.0 + 34.04 / 3600.0);
        close(position, "altitude", 519.5);
        assert_eq!(position.as_object().unwrap().len(), 3);
    }

    #[test]
    fn south_west_and_below_are_negative() {
        let fields = munich();
        let fields = with(fields, field(Tag::GPSLatitudeRef, ascii("S")));
        let fields = with(fields, field(Tag::GPSLongitudeRef, ascii("W")));
        let fields = with(fields, field(Tag::GPSAltitudeRef, Exif::Byte(vec![1])));
        let (findings, _) = examined(&fields);
        let position = &findings[0].1;
        close(position, "latitude", -(48.0 + 8.0 / 60.0 + 13.75 / 3600.0));
        close(
            position,
            "longitude",
            -(11.0 + 34.0 / 60.0 + 34.04 / 3600.0),
        );
        close(position, "altitude", -519.5);
    }

    #[test]
    fn a_zero_on_the_negative_side_is_zero() {
        let fields = with(munich(), field(Tag::GPSLatitudeRef, ascii("S")));
        let fields = with(
            fields,
            field(Tag::GPSLatitude, fractions(&[(0, 1), (0, 1), (0, 1)])),
        );
        let (findings, _) = examined(&fields);
        assert_eq!(findings[0].1["latitude"].to_string(), "0.0");
    }

    #[test]
    fn the_altitude_is_optional_and_its_reference_defaults_to_above() {
        let fields = without(without(munich(), Tag::GPSAltitude), Tag::GPSAltitudeRef);
        let (findings, remarks) = examined(&fields);
        assert!(
            findings[0].1.get("altitude").is_none(),
            "left out, not null"
        );
        assert_eq!(remarks, Vec::<String>::new());

        let (findings, _) = examined(&without(munich(), Tag::GPSAltitudeRef));
        close(&findings[0].1, "altitude", 519.5);

        let (findings, remarks) = examined(&with(
            munich(),
            field(Tag::GPSAltitudeRef, Exif::Byte(vec![7])),
        ));
        assert!(findings[0].1.get("altitude").is_none());
        assert_eq!(
            remarks,
            [
                "geo:position recorded without altitude, exif:gps-altitude-ref is not 0, 1, 2 or 3: 7"
            ]
        );
    }

    #[test]
    fn wgs84_in_any_spelling_or_no_datum_is_a_position() {
        for datum in ["WGS-84", "WGS84", "wgs 84", "WGS-1984", ""] {
            let (findings, remarks) =
                examined(&with(munich(), field(Tag::GPSMapDatum, ascii(datum))));
            assert_eq!(findings.len(), 1, "{datum:?}: {remarks:?}");
        }
    }

    #[test]
    fn what_cannot_be_used_is_reported_instead_of_recorded() {
        let cases = [
            (
                with(munich(), field(Tag::GPSMapDatum, ascii("TOKYO"))),
                "geo:position not recorded, exif:gps-map-datum is not WGS 84: TOKYO",
            ),
            (
                with(munich(), field(Tag::GPSStatus, ascii("V"))),
                "geo:position not recorded, exif:gps-status is V, the measurement is void",
            ),
            (
                without(munich(), Tag::GPSLatitudeRef),
                "geo:position not recorded, no exif:gps-latitude-ref",
            ),
            (
                with(munich(), field(Tag::GPSLongitudeRef, ascii("X"))),
                "geo:position not recorded, exif:gps-longitude-ref is not E or W: X",
            ),
            (
                with(
                    munich(),
                    field(Tag::GPSLatitude, fractions(&[(48, 1), (8, 1), (0, 0)])),
                ),
                "geo:position not recorded, exif:gps-latitude has a fraction with denominator 0: 48/1 8/1 0/0",
            ),
            (
                with(
                    munich(),
                    field(Tag::GPSLatitude, fractions(&[(48, 1), (8, 1)])),
                ),
                "geo:position not recorded, exif:gps-latitude is not three fractions: 48/1 8/1",
            ),
            (
                with(
                    munich(),
                    field(Tag::GPSLatitude, fractions(&[(95, 1), (0, 1), (0, 1)])),
                ),
                "geo:position not recorded, exif:gps-latitude is out of range: 95/1 0/1 0/1",
            ),
            (
                without(without(munich(), Tag::GPSLongitude), Tag::GPSLongitudeRef),
                "geo:position not recorded, no exif:gps-longitude",
            ),
        ];
        for (fields, remark) in cases {
            let (findings, remarks) = examined(&fields);
            assert_eq!(findings, Vec::new(), "{remark}");
            assert_eq!(remarks, [remark]);
        }
    }

    #[test]
    fn an_active_measurement_is_a_position() {
        let (findings, _) = examined(&with(munich(), field(Tag::GPSStatus, ascii("A"))));
        assert_eq!(findings.len(), 1);
    }

    #[test]
    fn exif_without_coordinates_is_an_empty_answer_and_no_report() {
        let only_the_date = [field(Tag::DateTimeOriginal, ascii("2019:07:14 11:02:41"))];
        assert_eq!(examined(&only_the_date), (Vec::new(), Vec::new()));
        let only_the_datum = [field(Tag::GPSMapDatum, ascii("WGS-84"))];
        assert_eq!(examined(&only_the_datum), (Vec::new(), Vec::new()));
    }
}
