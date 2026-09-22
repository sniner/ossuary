//! The ISO base media container behind HEIF, HEIC and AVIF, read from
//! its `meta` box alone: which item is the picture, what properties
//! describe it, where each item's bytes lie. No codestream is opened.
//! The `raster` contract asks it for the grid, the `xmp` contract for
//! the packet item.
//!
//! Boxes are sizes and four-letter types; the ones that matter here are
//! `pitm` (the primary item), `iinf` with its `infe` entries (what each
//! item is), `iprp` with `ipco` (the property boxes: `ispe` for the
//! grid, `pixi` for bits per channel, `auxC` for an auxiliary's role,
//! `hvcC` and `av1C` for the codec's own word on chroma and depth) and
//! `ipma` (which properties belong to which item), `iref` (`auxl`, an
//! alpha plane's tie to its picture; `dimg`, a grid's tiles) and `iloc`
//! with `idat` (where bytes lie).

use crate::raster::Raster;

pub struct Heif<'a> {
    bytes: &'a [u8],
    primary: u32,
    items: Vec<Item>,
    locations: Vec<Location>,
    properties: Vec<Property>,
    /// Item id and the one-based indexes into `properties` it carries.
    associations: Vec<(u32, Vec<u16>)>,
    references: Vec<Reference>,
    idat: Option<&'a [u8]>,
}

struct Item {
    id: u32,
    kind: [u8; 4],
    /// A `mime` item's declared type.
    content_type: Option<String>,
}

struct Location {
    item: u32,
    /// 0: offsets into the file; 1: into `idat`; 2: into another item.
    method: u8,
    base: u64,
    extents: Vec<(u64, u64)>,
}

struct Reference {
    kind: [u8; 4],
    from: u32,
    to: Vec<u32>,
}

enum Property {
    Ispe { width: u32, height: u32 },
    Pixi(Vec<u8>),
    AuxC(String),
    HvcC(Vec<u8>),
    Av1C(Vec<u8>),
    Other,
}

/// The container's table of contents; `None` where the bytes are no ISO
/// base media file with a `meta` box.
pub fn parse(bytes: &[u8]) -> Option<Heif<'_>> {
    let mut top = boxes(bytes);
    let (kind, _) = top.next()?;
    if &kind != b"ftyp" {
        return None;
    }
    let (_, meta) = top.find(|(kind, _)| kind == b"meta")?;
    let mut heif = Heif {
        bytes,
        primary: 0,
        items: Vec::new(),
        locations: Vec::new(),
        properties: Vec::new(),
        associations: Vec::new(),
        references: Vec::new(),
        idat: None,
    };
    let (_, _, children) = full_box(meta)?;
    for (kind, payload) in boxes(children) {
        match &kind {
            b"pitm" => heif.primary = pitm(payload)?,
            b"iinf" => heif.items = iinf(payload)?,
            b"iloc" => heif.locations = iloc(payload)?,
            b"iprp" => {
                for (kind, payload) in boxes(payload) {
                    match &kind {
                        b"ipco" => heif.properties = boxes(payload).map(property).collect(),
                        b"ipma" => heif.associations = ipma(payload)?,
                        _ => {}
                    }
                }
            }
            b"iref" => heif.references = iref(payload)?,
            b"idat" => heif.idat = Some(payload),
            _ => {}
        }
    }
    Some(heif)
}

impl Heif<'_> {
    /// The primary picture's grid: its `ispe` for the size, and for depth
    /// and colour the `pixi` or the codec configuration of the picture
    /// itself or, when the picture is a grid of tiles, of its first tile.
    /// Alpha is an auxiliary item tied to the picture with an alpha role.
    pub fn raster(&self) -> Option<Raster> {
        let primary = self.primary;
        let (width, height) = self.property(primary, |property| match property {
            Property::Ispe { width, height } => Some((*width, *height)),
            _ => None,
        })?;
        let coded: Vec<u32> = std::iter::once(primary)
            .chain(self.referenced(primary, *b"dimg"))
            .collect();
        let pixi = coded.iter().find_map(|&id| {
            self.property(id, |property| match property {
                Property::Pixi(bits) if !bits.is_empty() => Some(bits.clone()),
                _ => None,
            })
        });
        let hvcc = coded.iter().find_map(|&id| {
            self.property(id, |property| match property {
                Property::HvcC(record) => Some(record.clone()),
                _ => None,
            })
        });
        let av1c = coded.iter().find_map(|&id| {
            self.property(id, |property| match property {
                Property::Av1C(record) => Some(record.clone()),
                _ => None,
            })
        });
        // HEVCDecoderConfigurationRecord: chroma_format_idc at byte 16,
        // bit_depth_luma_minus8 at byte 17. AV1CodecConfigurationBox:
        // byte 2 holds high_bitdepth, twelve_bit and mono_chrome.
        let hvcc_gray = hvcc.as_ref().and_then(|r| r.get(16)).map(|b| b & 3) == Some(0);
        let hvcc_depth = hvcc.as_ref().and_then(|r| r.get(17)).map(|b| (b & 7) + 8);
        let av1c_gray = av1c
            .as_ref()
            .is_some_and(|r| r.get(2).is_some_and(|b| b & 0x10 != 0));
        let av1c_depth = av1c
            .as_ref()
            .and_then(|r| r.get(2))
            .map(|b| match (b & 0x40, b & 0x20) {
                (0, _) => 8,
                (_, 0) => 10,
                _ => 12,
            });
        let depth = pixi
            .as_ref()
            .and_then(|bits| bits.first().copied())
            .or(hvcc_depth)
            .or(av1c_depth)
            .unwrap_or(8);
        let gray = pixi.as_ref().is_some_and(|bits| bits.len() == 1) || hvcc_gray || av1c_gray;
        let alpha = self.references.iter().any(|reference| {
            &reference.kind == b"auxl"
                && reference.to.contains(&primary)
                && self
                    .property(reference.from, |property| match property {
                        // HEVC names its alpha plane by number, AVIF by word.
                        Property::AuxC(role)
                            if role == "urn:mpeg:hevc:2015:auxid:1" || role.contains("alpha") =>
                        {
                            Some(())
                        }
                        _ => None,
                    })
                    .is_some()
        });
        Some(Raster {
            width,
            height,
            depth,
            alpha,
            color: Some(if gray { "gray" } else { "rgb" }),
        })
    }

    /// The XMP packet: the `mime` item declared `application/rdf+xml`.
    pub fn xmp(&self) -> Option<Vec<u8>> {
        let item = self.items.iter().find(|item| {
            &item.kind == b"mime" && item.content_type.as_deref() == Some("application/rdf+xml")
        })?;
        self.payload(item.id)
    }

    /// An item's bytes, gathered from wherever its extents lie.
    fn payload(&self, id: u32) -> Option<Vec<u8>> {
        let location = self.locations.iter().find(|location| location.item == id)?;
        let source = match location.method {
            0 => self.bytes,
            1 => self.idat?,
            _ => return None,
        };
        let mut payload = Vec::new();
        for &(offset, length) in &location.extents {
            let start = usize::try_from(location.base.checked_add(offset)?).ok()?;
            let end = start.checked_add(usize::try_from(length).ok()?)?;
            payload.extend_from_slice(source.get(start..end)?);
        }
        Some(payload)
    }

    /// The first of an item's properties the picker accepts.
    fn property<T>(&self, id: u32, pick: impl Fn(&Property) -> Option<T>) -> Option<T> {
        let (_, indexes) = self.associations.iter().find(|(item, _)| *item == id)?;
        indexes
            .iter()
            .filter_map(|&index| self.properties.get(usize::from(index).checked_sub(1)?))
            .find_map(pick)
    }

    /// The items `from` refers to under `kind`, in order.
    fn referenced(&self, from: u32, kind: [u8; 4]) -> impl Iterator<Item = u32> + '_ {
        self.references
            .iter()
            .filter(move |reference| reference.from == from && reference.kind == kind)
            .flat_map(|reference| reference.to.iter().copied())
    }
}

/// The boxes of a run of bytes: type and payload, until one does not fit.
fn boxes(data: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> {
    let mut at = 0;
    std::iter::from_fn(move || {
        let header = data.get(at..at + 8)?;
        let kind = [header[4], header[5], header[6], header[7]];
        let (header_length, size) =
            match u32::from_be_bytes([header[0], header[1], header[2], header[3]]) {
                0 => (8, data.len() - at),
                1 => {
                    let large = data.get(at + 8..at + 16)?;
                    let mut bytes = [0; 8];
                    bytes.copy_from_slice(large);
                    (16, usize::try_from(u64::from_be_bytes(bytes)).ok()?)
                }
                size => (8, size as usize),
            };
        if size < header_length {
            return None;
        }
        let payload = data.get(at + header_length..at + size)?;
        at += size;
        Some((kind, payload))
    })
}

/// A full box's version, flags and the rest.
fn full_box(payload: &[u8]) -> Option<(u8, u32, &[u8])> {
    let head = payload.get(..4)?;
    let flags = u32::from_be_bytes([0, head[1], head[2], head[3]]);
    Some((head[0], flags, &payload[4..]))
}

/// A cursor over a box payload; every read is bounds-checked.
struct Bytes<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Bytes<'a> {
    fn new(data: &'a [u8]) -> Self {
        Bytes { data, at: 0 }
    }

    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let slice = self.data.get(self.at..self.at.checked_add(count)?)?;
        self.at += count;
        Some(slice)
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn u16(&mut self) -> Option<u16> {
        let bytes = self.take(2)?;
        Some(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&mut self) -> Option<u32> {
        let bytes = self.take(4)?;
        Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// An unsigned integer of 0, 4 or 8 bytes, as `iloc` sizes its fields.
    fn sized(&mut self, size: u8) -> Option<u64> {
        match size {
            0 => Some(0),
            4 => self.u32().map(u64::from),
            8 => {
                let bytes = self.take(8)?;
                let mut array = [0; 8];
                array.copy_from_slice(bytes);
                Some(u64::from_be_bytes(array))
            }
            _ => None,
        }
    }

    /// An id, two bytes in early versions and four later.
    fn id(&mut self, wide: bool) -> Option<u32> {
        if wide {
            self.u32()
        } else {
            self.u16().map(u32::from)
        }
    }

    fn kind(&mut self) -> Option<[u8; 4]> {
        let bytes = self.take(4)?;
        Some([bytes[0], bytes[1], bytes[2], bytes[3]])
    }

    /// A NUL-terminated string, the NUL consumed.
    fn cstring(&mut self) -> Option<String> {
        let rest = self.data.get(self.at..)?;
        let end = rest.iter().position(|&byte| byte == 0)?;
        let text = String::from_utf8_lossy(&rest[..end]).into_owned();
        self.at += end + 1;
        Some(text)
    }
}

fn pitm(payload: &[u8]) -> Option<u32> {
    let (version, _, rest) = full_box(payload)?;
    Bytes::new(rest).id(version >= 1)
}

fn iinf(payload: &[u8]) -> Option<Vec<Item>> {
    let (version, _, rest) = full_box(payload)?;
    let mut bytes = Bytes::new(rest);
    let count = if version == 0 {
        usize::from(bytes.u16()?)
    } else {
        bytes.u32()? as usize
    };
    let entries = bytes.take(rest.len() - bytes.at)?;
    let items = boxes(entries)
        .filter(|(kind, _)| kind == b"infe")
        .filter_map(|(_, payload)| infe(payload))
        .collect::<Vec<_>>();
    Some(items.into_iter().take(count).collect())
}

fn infe(payload: &[u8]) -> Option<Item> {
    let (version, _, rest) = full_box(payload)?;
    if version < 2 {
        // The old shape names no item type; nothing here to read.
        return None;
    }
    let mut bytes = Bytes::new(rest);
    let id = bytes.id(version >= 3)?;
    let _protection = bytes.u16()?;
    let kind = bytes.kind()?;
    let _name = bytes.cstring()?;
    let content_type = if &kind == b"mime" {
        bytes.cstring()
    } else {
        None
    };
    Some(Item {
        id,
        kind,
        content_type,
    })
}

fn iloc(payload: &[u8]) -> Option<Vec<Location>> {
    let (version, _, rest) = full_box(payload)?;
    let mut bytes = Bytes::new(rest);
    let sizes = bytes.u8()?;
    let (offset_size, length_size) = (sizes >> 4, sizes & 0xF);
    let sizes = bytes.u8()?;
    let (base_size, index_size) = (sizes >> 4, if version >= 1 { sizes & 0xF } else { 0 });
    let count = if version < 2 {
        usize::from(bytes.u16()?)
    } else {
        bytes.u32()? as usize
    };
    let mut locations = Vec::new();
    for _ in 0..count {
        let item = bytes.id(version >= 2)?;
        let method = if version >= 1 {
            (bytes.u16()? & 0xF) as u8
        } else {
            0
        };
        let _data_reference = bytes.u16()?;
        let base = bytes.sized(base_size)?;
        let extent_count = usize::from(bytes.u16()?);
        let mut extents = Vec::with_capacity(extent_count.min(64));
        for _ in 0..extent_count {
            if version >= 1 && index_size > 0 {
                bytes.sized(index_size)?;
            }
            let offset = bytes.sized(offset_size)?;
            let length = bytes.sized(length_size)?;
            extents.push((offset, length));
        }
        locations.push(Location {
            item,
            method,
            base,
            extents,
        });
    }
    Some(locations)
}

fn ipma(payload: &[u8]) -> Option<Vec<(u32, Vec<u16>)>> {
    let (version, flags, rest) = full_box(payload)?;
    let mut bytes = Bytes::new(rest);
    let count = bytes.u32()? as usize;
    let mut associations = Vec::new();
    for _ in 0..count {
        let item = bytes.id(version >= 1)?;
        let entries = usize::from(bytes.u8()?);
        let mut indexes = Vec::with_capacity(entries);
        for _ in 0..entries {
            indexes.push(if flags & 1 == 1 {
                bytes.u16()? & 0x7FFF
            } else {
                u16::from(bytes.u8()? & 0x7F)
            });
        }
        associations.push((item, indexes));
    }
    Some(associations)
}

fn iref(payload: &[u8]) -> Option<Vec<Reference>> {
    let (version, _, rest) = full_box(payload)?;
    let mut references = Vec::new();
    for (kind, payload) in boxes(rest) {
        let mut bytes = Bytes::new(payload);
        let Some(from) = bytes.id(version >= 1) else {
            continue;
        };
        let Some(count) = bytes.u16() else {
            continue;
        };
        let to = (0..count).filter_map(|_| bytes.id(version >= 1)).collect();
        references.push(Reference { kind, from, to });
    }
    Some(references)
}

fn property((kind, payload): ([u8; 4], &[u8])) -> Property {
    match &kind {
        b"ispe" => {
            let Some((_, _, rest)) = full_box(payload) else {
                return Property::Other;
            };
            let mut bytes = Bytes::new(rest);
            match (bytes.u32(), bytes.u32()) {
                (Some(width), Some(height)) => Property::Ispe { width, height },
                _ => Property::Other,
            }
        }
        b"pixi" => {
            let Some((_, _, rest)) = full_box(payload) else {
                return Property::Other;
            };
            let mut bytes = Bytes::new(rest);
            let count = bytes.u8().unwrap_or(0);
            let bits = bytes.take(usize::from(count)).unwrap_or(&[]).to_vec();
            Property::Pixi(bits)
        }
        b"auxC" => match full_box(payload).and_then(|(_, _, rest)| Bytes::new(rest).cstring()) {
            Some(role) => Property::AuxC(role),
            None => Property::Other,
        },
        b"hvcC" => Property::HvcC(payload.to_vec()),
        b"av1C" => Property::Av1C(payload.to_vec()),
        _ => Property::Other,
    }
}

#[cfg(test)]
pub mod builder {
    //! The boxes of a HEIF, assembled by hand for the tests.

    pub fn bx(kind: [u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = (u32::try_from(payload.len() + 8).unwrap())
            .to_be_bytes()
            .to_vec();
        out.extend_from_slice(&kind);
        out.extend_from_slice(payload);
        out
    }

    pub fn full(kind: [u8; 4], version: u8, flags: u32, payload: &[u8]) -> Vec<u8> {
        let mut body = vec![version];
        body.extend_from_slice(&flags.to_be_bytes()[1..]);
        body.extend_from_slice(payload);
        bx(kind, &body)
    }

    pub fn ftyp(brand: [u8; 4]) -> Vec<u8> {
        let mut body = brand.to_vec();
        body.extend_from_slice(&[0, 0, 0, 0]);
        body.extend_from_slice(b"mif1");
        bx(*b"ftyp", &body)
    }

    pub fn pitm(id: u16) -> Vec<u8> {
        full(*b"pitm", 0, 0, &id.to_be_bytes())
    }

    pub fn infe(id: u16, kind: [u8; 4], content_type: Option<&str>) -> Vec<u8> {
        let mut body = id.to_be_bytes().to_vec();
        body.extend_from_slice(&[0, 0]);
        body.extend_from_slice(&kind);
        body.push(0);
        if let Some(content_type) = content_type {
            body.extend_from_slice(content_type.as_bytes());
            body.push(0);
        }
        full(*b"infe", 2, 0, &body)
    }

    pub fn iinf(entries: &[Vec<u8>]) -> Vec<u8> {
        let mut body = (u16::try_from(entries.len()).unwrap())
            .to_be_bytes()
            .to_vec();
        for entry in entries {
            body.extend_from_slice(entry);
        }
        full(*b"iinf", 0, 0, &body)
    }

    pub fn ispe(width: u32, height: u32) -> Vec<u8> {
        let mut body = width.to_be_bytes().to_vec();
        body.extend_from_slice(&height.to_be_bytes());
        full(*b"ispe", 0, 0, &body)
    }

    pub fn pixi(bits: &[u8]) -> Vec<u8> {
        let mut body = vec![u8::try_from(bits.len()).unwrap()];
        body.extend_from_slice(bits);
        full(*b"pixi", 0, 0, &body)
    }

    pub fn auxc(role: &str) -> Vec<u8> {
        let mut body = role.as_bytes().to_vec();
        body.push(0);
        full(*b"auxC", 0, 0, &body)
    }

    /// An HEVC configuration record with the chroma format and luma depth given.
    pub fn hvcc(chroma: u8, depth: u8) -> Vec<u8> {
        let mut record = vec![1; 23];
        record[16] = 0xFC | chroma;
        record[17] = 0xF8 | (depth - 8);
        bx(*b"hvcC", &record)
    }

    pub fn av1c(mono: bool, high: bool, twelve: bool) -> Vec<u8> {
        let flags = if mono { 0x10 } else { 0 }
            | if high { 0x40 } else { 0 }
            | if twelve { 0x20 } else { 0 };
        bx(*b"av1C", &[0x81, 0, flags, 0])
    }

    pub fn ipma(entries: &[(u16, &[u8])]) -> Vec<u8> {
        let mut body = (u32::try_from(entries.len()).unwrap())
            .to_be_bytes()
            .to_vec();
        for (item, indexes) in entries {
            body.extend_from_slice(&item.to_be_bytes());
            body.push(u8::try_from(indexes.len()).unwrap());
            body.extend_from_slice(indexes);
        }
        full(*b"ipma", 0, 0, &body)
    }

    pub fn iprp(properties: &[Vec<u8>], ipma: &[u8]) -> Vec<u8> {
        let ipco = bx(*b"ipco", &properties.concat());
        bx(*b"iprp", &[ipco, ipma.to_vec()].concat())
    }

    pub fn iref(references: &[([u8; 4], u16, &[u16])]) -> Vec<u8> {
        let mut body = Vec::new();
        for (kind, from, to) in references {
            let mut entry = from.to_be_bytes().to_vec();
            entry.extend_from_slice(&(u16::try_from(to.len()).unwrap()).to_be_bytes());
            for id in *to {
                entry.extend_from_slice(&id.to_be_bytes());
            }
            body.extend(bx(*kind, &entry));
        }
        full(*b"iref", 0, 0, &body)
    }

    /// Version 1 `iloc` with four-byte offsets and lengths, no base offset.
    pub fn iloc(entries: &[(u16, u8, u32, u32)]) -> Vec<u8> {
        let mut body = vec![0x44, 0x00];
        body.extend_from_slice(&(u16::try_from(entries.len()).unwrap()).to_be_bytes());
        for (item, method, offset, length) in entries {
            body.extend_from_slice(&item.to_be_bytes());
            body.extend_from_slice(&u16::from(*method).to_be_bytes());
            body.extend_from_slice(&[0, 0]);
            body.extend_from_slice(&[0, 1]);
            body.extend_from_slice(&offset.to_be_bytes());
            body.extend_from_slice(&length.to_be_bytes());
        }
        full(*b"iloc", 1, 0, &body)
    }

    pub fn meta(children: &[Vec<u8>]) -> Vec<u8> {
        let hdlr = full(
            *b"hdlr",
            0,
            0,
            &[
                0, 0, 0, 0, b'p', b'i', b'c', b't', 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            ],
        );
        full(*b"meta", 0, 0, &[hdlr, children.concat()].concat())
    }
}

#[cfg(test)]
mod tests {
    use super::builder::*;
    use super::{Raster, parse};

    /// An iPhone-style HEIC: a ten-bit picture with an alpha plane and
    /// an XMP packet in `idat`.
    fn heic(xmp: &[u8]) -> Vec<u8> {
        let meta = meta(&[
            pitm(1),
            iinf(&[
                infe(1, *b"hvc1", None),
                infe(2, *b"hvc1", None),
                infe(3, *b"mime", Some("application/rdf+xml")),
            ]),
            iref(&[(*b"auxl", 2, &[1])]),
            iprp(
                &[
                    ispe(4032, 3024),
                    pixi(&[10, 10, 10]),
                    hvcc(1, 10),
                    auxc("urn:mpeg:hevc:2015:auxid:1"),
                    pixi(&[10]),
                ],
                &ipma(&[(1, &[1, 2, 3]), (2, &[4, 5, 1])]),
            ),
            iloc(&[(3, 1, 0, u32::try_from(xmp.len()).unwrap())]),
            bx(*b"idat", xmp),
        ]);
        [ftyp(*b"heic"), meta, bx(*b"mdat", &[0; 16])].concat()
    }

    #[test]
    fn the_primary_items_grid_with_its_alpha_plane() {
        let file = heic(b"<x/>");
        assert_eq!(
            parse(&file).unwrap().raster(),
            Some(Raster {
                width: 4032,
                height: 3024,
                depth: 10,
                alpha: true,
                color: Some("rgb"),
            })
        );
        assert_eq!(
            crate::raster::read(&file).map(|raster| raster.depth),
            Some(10)
        );
    }

    #[test]
    fn the_xmp_item_is_read_from_idat() {
        let file = heic(b"<packet/>");
        assert_eq!(
            parse(&file).unwrap().xmp().as_deref(),
            Some(b"<packet/>".as_slice())
        );
    }

    #[test]
    fn the_xmp_item_is_read_from_the_file_by_offset() {
        let packet = b"<by-offset/>";
        let build = |offset: u32| {
            let meta = meta(&[
                pitm(1),
                iinf(&[
                    infe(1, *b"hvc1", None),
                    infe(2, *b"mime", Some("application/rdf+xml")),
                ]),
                iprp(&[ispe(1, 1)], &ipma(&[(1, &[1])])),
                iloc(&[(2, 0, offset, u32::try_from(packet.len()).unwrap())]),
            ]);
            [ftyp(*b"heic"), meta, bx(*b"mdat", packet)].concat()
        };
        // The offset of mdat's payload does not depend on the offset written.
        let offset = u32::try_from(build(0).len() - packet.len()).unwrap();
        assert_eq!(
            parse(&build(offset)).unwrap().xmp().as_deref(),
            Some(packet.as_slice())
        );
    }

    #[test]
    fn a_grid_answers_from_its_tiles_and_the_codec_speaks_where_pixi_is_silent() {
        let meta = meta(&[
            pitm(1),
            iinf(&[
                infe(1, *b"grid", None),
                infe(2, *b"hvc1", None),
                infe(3, *b"hvc1", None),
            ]),
            iref(&[(*b"dimg", 1, &[2, 3])]),
            iprp(
                &[ispe(8000, 6000), hvcc(0, 8), ispe(4000, 6000)],
                &ipma(&[(1, &[1]), (2, &[2, 3]), (3, &[2, 3])]),
            ),
        ]);
        let file = [ftyp(*b"heic"), meta].concat();
        assert_eq!(
            parse(&file).unwrap().raster(),
            Some(Raster {
                width: 8000,
                height: 6000,
                depth: 8,
                alpha: false,
                color: Some("gray"),
            }),
            "the grid's own size, the tile's chroma format"
        );
    }

    #[test]
    fn an_avif_says_its_depth_through_av1c() {
        let meta = meta(&[
            pitm(1),
            iinf(&[infe(1, *b"av01", None)]),
            iprp(
                &[ispe(64, 64), av1c(false, true, false)],
                &ipma(&[(1, &[1, 2])]),
            ),
        ]);
        let file = [ftyp(*b"avif"), meta].concat();
        let raster = parse(&file).unwrap().raster().unwrap();
        assert_eq!((raster.depth, raster.color), (10, Some("rgb")));
    }

    #[test]
    fn bytes_that_are_no_heif_are_an_empty_answer() {
        assert!(parse(b"plain words").is_none());
        assert!(parse(&ftyp(*b"isom")).is_none(), "a movie has no meta box");
        let torn = &heic(b"<x/>")[..40];
        assert!(parse(torn).is_none() || parse(torn).unwrap().raster().is_none());
        let no_ispe = [
            ftyp(*b"heic"),
            meta(&[pitm(1), iinf(&[infe(1, *b"hvc1", None)])]),
        ]
        .concat();
        assert!(parse(&no_ispe).unwrap().raster().is_none());
        assert!(parse(&no_ispe).unwrap().xmp().is_none());
    }
}
