//! The `xmp` contract: every property of the file's XMP packet, in
//! XMP's own words — the schema's prefix and the property's name
//! kebab-cased and joined under `xmp:`, the value as the packet spells
//! it. `dc:subject` becomes `xmp:dc-subject`, `xmpMM:DocumentID`
//! `xmp:xmp-mm-document-id`, `photoshop:City` `xmp:photoshop-city`.
//!
//! XMP is RDF, and RDF nests: a property may be one value, an ordered
//! or unordered list of values (`rdf:Seq`, `rdf:Bag`), a choice of
//! alternatives by language (`rdf:Alt`), or a structure of further
//! properties, in any combination. This contract flattens it all into
//! attributes: a list's items each stand as a value of the same
//! attribute; of alternatives the default language (`x-default`) is
//! taken, the first one where none is marked; a structure's fields join
//! the path with a dash, so `Iptc4xmpExt:LocationCreated`'s
//! `Iptc4xmpExt:City` stands as
//! `xmp:iptc4xmp-ext-location-created-iptc4xmp-ext-city`. Values are
//! the packet's text, untouched: `"5"` stays text, a date keeps its
//! spelling.
//!
//! The packet's prefixes are the specification's, not the file's: a
//! writer that spells the Dublin Core namespace `dcterms` instead of
//! `dc` still lands on `xmp:dc-…`, since the namespace URI is what the
//! two have in common. A namespace this program does not know keeps the
//! prefix the packet declared for it.

use roxmltree::{Document, Node, NodeType};
use serde_json::{Value, json};

use crate::format::{self, Kind};

const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const XML: &str = "http://www.w3.org/XML/1998/namespace";

/// The namespaces the XMP specification and its neighbours name, with
/// the prefix each is known by.
const PREFIXES: &[(&str, &str)] = &[
    ("http://purl.org/dc/elements/1.1/", "dc"),
    ("http://ns.adobe.com/xap/1.0/", "xmp"),
    ("http://ns.adobe.com/xap/1.0/rights/", "xmpRights"),
    ("http://ns.adobe.com/xap/1.0/mm/", "xmpMM"),
    ("http://ns.adobe.com/xap/1.0/bj/", "xmpBJ"),
    ("http://ns.adobe.com/xap/1.0/t/pg/", "xmpTPg"),
    ("http://ns.adobe.com/xmp/1.0/DynamicMedia/", "xmpDM"),
    ("http://ns.adobe.com/xmp/note/", "xmpNote"),
    ("http://ns.adobe.com/xmp/Identifier/qual/1.0/", "xmpidq"),
    ("http://ns.adobe.com/xap/1.0/sType/ResourceEvent#", "stEvt"),
    ("http://ns.adobe.com/xap/1.0/sType/ResourceRef#", "stRef"),
    ("http://ns.adobe.com/xap/1.0/sType/Dimensions#", "stDim"),
    ("http://ns.adobe.com/xap/1.0/sType/Version#", "stVer"),
    ("http://ns.adobe.com/xap/1.0/sType/Job#", "stJob"),
    ("http://ns.adobe.com/pdf/1.3/", "pdf"),
    ("http://ns.adobe.com/photoshop/1.0/", "photoshop"),
    ("http://ns.adobe.com/camera-raw-settings/1.0/", "crs"),
    ("http://ns.adobe.com/tiff/1.0/", "tiff"),
    ("http://ns.adobe.com/exif/1.0/", "exif"),
    ("http://cipa.jp/exif/1.0/", "exifEX"),
    ("http://ns.adobe.com/exif/1.0/aux/", "aux"),
    (
        "http://iptc.org/std/Iptc4xmpCore/1.0/xmlns/",
        "Iptc4xmpCore",
    ),
    ("http://iptc.org/std/Iptc4xmpExt/2008-02-29/", "Iptc4xmpExt"),
    ("http://ns.useplus.org/ldf/xmp/1.0/", "plus"),
    ("http://ns.adobe.com/lightroom/1.0/", "lr"),
    ("http://ns.google.com/photos/1.0/panorama/", "GPano"),
    ("http://ns.google.com/photos/1.0/camera/", "GCamera"),
    ("http://ns.microsoft.com/photo/1.0/", "MicrosoftPhoto"),
    ("http://ns.microsoft.com/photo/1.2/", "MP"),
    ("http://www.digikam.org/ns/1.0/", "digiKam"),
    ("http://ns.adobe.com/hdr-gain-map/1.0/", "hdrgm"),
    ("http://ns.apple.com/pixeldatainfo/1.0/", "apdi"),
    (
        "http://www.metadataworkinggroup.com/schemas/regions/",
        "mwg-rs",
    ),
    ("http://ns.adobe.com/xmp/sType/Area#", "stArea"),
];

/// The findings of the XMP packets these bytes carry: one attribute
/// per property, one value bare and several as a list.
pub fn read(bytes: &[u8]) -> Vec<(String, Value)> {
    let mut findings = Findings::default();
    for packet in packets(bytes) {
        findings.read(&packet);
    }
    findings.into_findings("xmp")
}

/// The XMP packets, where the container keeps them: a JPEG's APP1
/// segment and, when the packet outgrew it, its extension; a PNG's
/// `iTXt` chunk keyed `XML:com.adobe.xmp`; a TIFF's tag 700; a WebP's
/// `XMP ` chunk; a HEIF's `mime` item of type `application/rdf+xml`.
fn packets(bytes: &[u8]) -> Vec<Vec<u8>> {
    match format::sniff(bytes) {
        Some(Kind::Jpeg) => {
            let Some(decoder) = format::jpeg_headers(bytes) else {
                return Vec::new();
            };
            let Some(info) = decoder.info() else {
                return Vec::new();
            };
            [info.xmp_data, info.extended_xmp]
                .into_iter()
                .flatten()
                .collect()
        }
        Some(Kind::Png) => {
            let Ok(reader) = png::Decoder::new(std::io::Cursor::new(bytes)).read_info() else {
                return Vec::new();
            };
            reader
                .info()
                .utf8_text
                .iter()
                .filter(|chunk| chunk.keyword == "XML:com.adobe.xmp")
                .filter_map(|chunk| chunk.get_text().ok())
                .map(String::into_bytes)
                .collect()
        }
        Some(Kind::Tiff) => {
            let Ok(mut decoder) = tiff::decoder::Decoder::new(std::io::Cursor::new(bytes)) else {
                return Vec::new();
            };
            let Ok(Some(value)) = decoder.find_tag(tiff::tags::Tag::Unknown(700)) else {
                return Vec::new();
            };
            let packet = match value {
                tiff::decoder::ifd::Value::Ascii(text) => Some(text.into_bytes()),
                other => other.into_u8_vec().ok(),
            };
            packet.into_iter().collect()
        }
        Some(Kind::WebP) => {
            let Ok(mut decoder) = image_webp::WebPDecoder::new(std::io::Cursor::new(bytes)) else {
                return Vec::new();
            };
            decoder.xmp_metadata().ok().flatten().into_iter().collect()
        }
        Some(Kind::Heif) => crate::heif::parse(bytes)
            .and_then(|heif| heif.xmp())
            .into_iter()
            .collect(),
        None => Vec::new(),
    }
}

/// Attributes in the order first met, each with every value said for it.
#[derive(Default)]
struct Findings(Vec<(String, Vec<String>)>);

impl Findings {
    fn add(&mut self, attribute: &str, value: &str) {
        if let Some((_, values)) = self.0.iter_mut().find(|(name, _)| name == attribute) {
            values.push(value.to_string());
        } else {
            self.0
                .push((attribute.to_string(), vec![value.to_string()]));
        }
    }

    fn into_findings(self, namespace: &str) -> Vec<(String, Value)> {
        self.0
            .into_iter()
            .map(|(attribute, mut values)| {
                let value = if values.len() == 1 {
                    json!(values.remove(0))
                } else {
                    json!(values)
                };
                (format!("{namespace}:{attribute}"), value)
            })
            .collect()
    }

    /// Every property of every `rdf:Description` directly under the
    /// packet's `rdf:RDF`.
    fn read(&mut self, packet: &[u8]) {
        let text = String::from_utf8_lossy(packet);
        let text = text.trim_matches(|c: char| c == '\0' || c.is_whitespace());
        let Ok(document) = Document::parse(text) else {
            return;
        };
        let descriptions = document.descendants().filter(|node| {
            is_rdf(node, "Description")
                && node.parent().is_some_and(|parent| is_rdf(&parent, "RDF"))
        });
        for description in descriptions {
            self.properties(description, "");
        }
    }

    /// The fields of a structure: property attributes and property
    /// elements, each under `path` extended by its own name.
    fn properties(&mut self, node: Node, path: &str) {
        for attribute in node.attributes() {
            let Some(namespace) = attribute.namespace() else {
                continue;
            };
            if namespace == RDF || namespace == XML {
                continue;
            }
            let Some(name) = attribute_name(&node, path, namespace, attribute.name()) else {
                continue;
            };
            self.add(&name, attribute.value());
        }
        for child in node.children().filter(Node::is_element) {
            let Some(namespace) = child.tag_name().namespace() else {
                continue;
            };
            if namespace == RDF {
                continue;
            }
            let Some(name) = attribute_name(&child, path, namespace, child.tag_name().name())
            else {
                continue;
            };
            self.value(child, &name);
        }
    }

    /// One property's value, whatever shape RDF gave it.
    fn value(&mut self, node: Node, name: &str) {
        let elements: Vec<Node> = node.children().filter(Node::is_element).collect();
        if let Some(value) = elements.iter().find(|child| is_rdf(child, "value")) {
            // A qualified value: the value itself, its qualifiers aside.
            self.value(*value, name);
            return;
        }
        if elements.is_empty() {
            self.properties(node, name);
            if let Some(text) = node.text().map(str::trim).filter(|text| !text.is_empty()) {
                self.add(name, text);
            }
            return;
        }
        if let [container] = elements.as_slice() {
            if is_rdf(container, "Bag") || is_rdf(container, "Seq") {
                for item in container.children().filter(|item| is_rdf(item, "li")) {
                    self.value(item, name);
                }
                return;
            }
            if is_rdf(container, "Alt") {
                let items: Vec<Node> = container
                    .children()
                    .filter(|item| is_rdf(item, "li"))
                    .collect();
                let default = items
                    .iter()
                    .find(|item| item.attribute((XML, "lang")) == Some("x-default"))
                    .or(items.first());
                if let Some(item) = default {
                    self.value(*item, name);
                }
                return;
            }
            if is_rdf(container, "Description") {
                self.properties(*container, name);
                return;
            }
        }
        // A structure spelled inline, `rdf:parseType="Resource"` or not.
        self.properties(node, name);
    }
}

fn is_rdf(node: &Node, name: &str) -> bool {
    node.node_type() == NodeType::Element
        && node.tag_name().namespace() == Some(RDF)
        && node.tag_name().name() == name
}

/// The attribute path for a property: the prefix its namespace is known
/// by and its own name, kebab-cased, joined to the path of the structure
/// it lives in. `None` for a name that leaves nothing legal to say.
fn attribute_name(node: &Node, path: &str, namespace: &str, local: &str) -> Option<String> {
    let prefix = PREFIXES
        .iter()
        .find(|(uri, _)| *uri == namespace)
        .map(|(_, prefix)| *prefix)
        .or_else(|| node.lookup_prefix(namespace))
        .unwrap_or("");
    let own = if prefix.is_empty() {
        kebab(local)
    } else {
        format!("{}-{}", kebab(prefix), kebab(local))
    };
    let own = legal(&own);
    if own.is_empty() {
        return None;
    }
    Some(if path.is_empty() {
        own
    } else {
        format!("{path}-{own}")
    })
}

/// The name reduced to the claim grammar: lowercase `a-z`, `0-9`, `-`,
/// with what was a separator in the source kept as a dash.
fn legal(name: &str) -> String {
    let mut result = String::with_capacity(name.len());
    for character in name.chars() {
        match character {
            'a'..='z' | '0'..='9' => result.push(character),
            'A'..='Z' => result.push(character.to_ascii_lowercase()),
            '-' | '_' | '.' | ':' | ' ' if !result.is_empty() && !result.ends_with('-') => {
                result.push('-');
            }
            _ => {}
        }
    }
    result.trim_end_matches('-').to_string()
}

/// `DocumentID` → `document-id`, `xmpMM` → `xmp-mm`, `Iptc4xmpCore` →
/// `iptc4xmp-core`: the same word boundaries the `exif` contract draws.
fn kebab(name: &str) -> String {
    crate::kebab(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(body: &str) -> Vec<u8> {
        format!(
            r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  {body}
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#
        )
        .into_bytes()
    }

    fn findings(body: &str) -> Vec<(String, Value)> {
        let mut findings = Findings::default();
        findings.read(&packet(body));
        findings.into_findings("xmp")
    }

    #[test]
    fn simple_properties_as_elements_and_as_attributes() {
        let found = findings(
            r#"<rdf:Description rdf:about=""
      xmlns:xmp="http://ns.adobe.com/xap/1.0/"
      xmlns:photoshop="http://ns.adobe.com/photoshop/1.0/"
      xmp:Rating="5">
    <xmp:CreateDate>2019-07-14T11:02:41+02:00</xmp:CreateDate>
    <photoshop:City>Wien</photoshop:City>
   </rdf:Description>"#,
        );
        assert_eq!(
            found,
            vec![
                ("xmp:xmp-rating".to_string(), json!("5")),
                (
                    "xmp:xmp-create-date".to_string(),
                    json!("2019-07-14T11:02:41+02:00")
                ),
                ("xmp:photoshop-city".to_string(), json!("Wien")),
            ],
            "text stays text, rdf:about is no property"
        );
    }

    #[test]
    fn a_bag_is_a_list_and_an_alt_is_its_default_language() {
        let found = findings(
            r#"<rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:subject><rdf:Bag><rdf:li>alps</rdf:li><rdf:li>summer</rdf:li></rdf:Bag></dc:subject>
    <dc:title><rdf:Alt>
      <rdf:li xml:lang="en">The Alps</rdf:li>
      <rdf:li xml:lang="x-default">Die Alpen</rdf:li>
    </rdf:Alt></dc:title>
    <dc:creator><rdf:Seq><rdf:li>Someone</rdf:li></rdf:Seq></dc:creator>
   </rdf:Description>"#,
        );
        assert_eq!(
            found,
            vec![
                ("xmp:dc-subject".to_string(), json!(["alps", "summer"])),
                ("xmp:dc-title".to_string(), json!("Die Alpen")),
                ("xmp:dc-creator".to_string(), json!("Someone")),
            ],
            "several as a list, one bare, the default language of the alternatives"
        );
    }

    #[test]
    fn an_alt_without_a_default_takes_its_first() {
        let found = findings(
            r#"<rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:description><rdf:Alt>
      <rdf:li xml:lang="de">Erste</rdf:li>
      <rdf:li xml:lang="fr">Deuxième</rdf:li>
    </rdf:Alt></dc:description>
   </rdf:Description>"#,
        );
        assert_eq!(
            found,
            vec![("xmp:dc-description".to_string(), json!("Erste"))]
        );
    }

    #[test]
    fn structures_fold_into_the_path() {
        let found = findings(
            r#"<rdf:Description rdf:about=""
      xmlns:Iptc4xmpExt="http://iptc.org/std/Iptc4xmpExt/2008-02-29/"
      xmlns:xmpMM="http://ns.adobe.com/xap/1.0/mm/"
      xmlns:stEvt="http://ns.adobe.com/xap/1.0/sType/ResourceEvent#">
    <Iptc4xmpExt:LocationCreated><rdf:Bag><rdf:li rdf:parseType="Resource">
      <Iptc4xmpExt:City>Wien</Iptc4xmpExt:City>
      <Iptc4xmpExt:CountryName>Österreich</Iptc4xmpExt:CountryName>
    </rdf:li></rdf:Bag></Iptc4xmpExt:LocationCreated>
    <xmpMM:History><rdf:Seq>
      <rdf:li stEvt:action="created" stEvt:softwareAgent="Example 1.0"/>
      <rdf:li stEvt:action="saved" stEvt:softwareAgent="Example 1.1"/>
    </rdf:Seq></xmpMM:History>
    <xmpMM:DerivedFrom><rdf:Description>
      <stEvt:action>converted</stEvt:action>
    </rdf:Description></xmpMM:DerivedFrom>
   </rdf:Description>"#,
        );
        assert_eq!(
            found,
            vec![
                (
                    "xmp:iptc4xmp-ext-location-created-iptc4xmp-ext-city".to_string(),
                    json!("Wien")
                ),
                (
                    "xmp:iptc4xmp-ext-location-created-iptc4xmp-ext-country-name".to_string(),
                    json!("Österreich")
                ),
                (
                    "xmp:xmp-mm-history-st-evt-action".to_string(),
                    json!(["created", "saved"])
                ),
                (
                    "xmp:xmp-mm-history-st-evt-software-agent".to_string(),
                    json!(["Example 1.0", "Example 1.1"])
                ),
                (
                    "xmp:xmp-mm-derived-from-st-evt-action".to_string(),
                    json!("converted")
                ),
            ],
            "fields of a structure, in a list or not, join the path; the list's items pile up on the same attribute"
        );
    }

    #[test]
    fn the_prefix_is_the_specifications_not_the_files() {
        let found = findings(
            r#"<rdf:Description rdf:about=""
      xmlns:dcterms="http://purl.org/dc/elements/1.1/"
      xmlns:odd="http://example.com/odd/1.0/">
    <dcterms:format>image/jpeg</dcterms:format>
    <odd:Some_Thing>x</odd:Some_Thing>
   </rdf:Description>"#,
        );
        assert_eq!(
            found,
            vec![
                ("xmp:dc-format".to_string(), json!("image/jpeg")),
                ("xmp:odd-some-thing".to_string(), json!("x")),
            ],
            "a known namespace by its own prefix; an unknown one by the packet's, made legal"
        );
    }

    #[test]
    fn a_qualified_value_is_the_value() {
        let found = findings(
            r#"<rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/"
      xmlns:xmpidq="http://ns.adobe.com/xmp/Identifier/qual/1.0/">
    <dc:identifier rdf:parseType="Resource">
      <rdf:value>urn:example:1</rdf:value>
      <xmpidq:Scheme>urn</xmpidq:Scheme>
    </dc:identifier>
   </rdf:Description>"#,
        );
        assert_eq!(
            found,
            vec![("xmp:dc-identifier".to_string(), json!("urn:example:1"))]
        );
    }

    #[test]
    fn two_descriptions_add_up_and_nested_ones_are_not_roots() {
        let found = findings(
            r#"<rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:format>image/png</dc:format>
   </rdf:Description>
   <rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/">
    <xmp:Rating>3</xmp:Rating>
   </rdf:Description>"#,
        );
        assert_eq!(found.len(), 2);
    }

    #[test]
    fn bytes_without_a_packet_are_an_empty_answer() {
        assert_eq!(read(b"plain words"), Vec::new());
        assert_eq!(read(&[]), Vec::new());
        let mut findings = Findings::default();
        findings.read(b"<not xml");
        assert_eq!(findings.into_findings("xmp"), Vec::new());
    }

    #[test]
    fn names_are_reduced_to_the_claim_grammar() {
        assert_eq!(legal("xmp-mm-Document_ID"), "xmp-mm-document-id");
        assert_eq!(legal("--odd..name--"), "odd-name");
        assert_eq!(legal("ünïcode"), "ncode");
        assert_eq!(legal("###"), "");
    }

    fn jpeg_with_xmp(packet: &[u8]) -> Vec<u8> {
        let mut segment = b"http://ns.adobe.com/xap/1.0/\0".to_vec();
        segment.extend_from_slice(packet);
        let mut out = Vec::new();
        let mut encoder = jpeg_encoder::Encoder::new(&mut out, 90);
        encoder.add_app_segment(1, &segment).unwrap();
        encoder
            .encode(&[0; 12], 2, 2, jpeg_encoder::ColorType::Rgb)
            .unwrap();
        out
    }

    const BODY: &str = r#"<rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:subject><rdf:Bag><rdf:li>tagged</rdf:li></rdf:Bag></dc:subject>
   </rdf:Description>"#;

    #[test]
    fn a_jpeg_carries_its_packet_in_app1() {
        assert_eq!(
            read(&jpeg_with_xmp(&packet(BODY))),
            vec![("xmp:dc-subject".to_string(), json!("tagged"))]
        );
    }

    #[test]
    fn a_png_carries_its_packet_in_itxt() {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, 1, 1);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .add_itxt_chunk(
                "XML:com.adobe.xmp".to_string(),
                String::from_utf8(packet(BODY)).unwrap(),
            )
            .unwrap();
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[0]).unwrap();
        drop(writer);
        assert_eq!(
            read(&out),
            vec![("xmp:dc-subject".to_string(), json!("tagged"))]
        );
    }

    #[test]
    fn a_tiff_carries_its_packet_in_tag_700() {
        let mut out = std::io::Cursor::new(Vec::new());
        let mut encoder = tiff::encoder::TiffEncoder::new(&mut out).unwrap();
        let mut image = encoder
            .new_image::<tiff::encoder::colortype::Gray8>(1, 1)
            .unwrap();
        image
            .encoder()
            .write_tag(tiff::tags::Tag::Unknown(700), packet(BODY).as_slice())
            .unwrap();
        image.write_data(&[0u8]).unwrap();
        assert_eq!(
            read(&out.into_inner()),
            vec![("xmp:dc-subject".to_string(), json!("tagged"))]
        );
    }

    #[test]
    fn a_webp_carries_its_packet_in_its_xmp_chunk() {
        let mut out = Vec::new();
        let mut encoder = image_webp::WebPEncoder::new(&mut out);
        encoder.set_xmp_metadata(packet(BODY));
        encoder
            .encode(&[0; 3], 1, 1, image_webp::ColorType::Rgb8)
            .unwrap();
        assert_eq!(
            read(&out),
            vec![("xmp:dc-subject".to_string(), json!("tagged"))]
        );
    }
}
