//! Streaming reader for the B2S backglass XML (`.directb2s` / `.b2spro`).
//!
//! The image payloads are 5-19 MB of base64 sitting in the `Value` attribute of
//! `Images/BackglassImage` and `Images/DMDImage`, so the file is streamed and
//! the decoded pixels are only kept when hole detection is asked for.

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use quick_xml::events::Event;
use quick_xml::{Reader, XmlVersion};

use crate::b2s::Rect;

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Size {
    pub w: u32,
    pub h: u32,
}

impl std::fmt::Display for Size {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}x{}", self.w, self.h)
    }
}

/// The `<DMD>` element inside `<Displays>`, in backglass image pixels.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct DmdTag {
    pub target: String,
    pub rect: Rect,
    pub glass_roughness: Option<f64>,
}

#[derive(Clone, Debug)]
pub struct ImageInfo {
    /// file name of the designer's source image, from the `FileName` attribute.
    /// Kept for diagnosis: it says what the author started from, which is the
    /// first thing to look at when a detected hole looks wrong
    #[allow(dead_code)]
    pub source_file_name: Option<String>,
    pub size: Option<Size>,
    /// length of the base64 payload, a cheap way to tell a stub image from a real one
    #[allow(dead_code)]
    pub base64_len: usize,
}

/// One image element inside `<Images>`: the element name (`BackglassImage`,
/// `DMDImage`, `ThumbnailImage`, ...), the designer's source file name, and
/// the payload decoded from base64 when `want_images` was set.
#[derive(Debug)]
pub struct B2sImage {
    pub element: String,
    pub source_file_name: Option<String>,
    /// length of the base64 payload, a cheap way to tell a stub image from a real one
    pub base64_len: usize,
    pub size: Option<Size>,
    /// decoded image bytes, only present when `want_images` was set and the
    /// payload decoded; `None` for an empty or corrupt base64 value
    pub bytes: Option<Vec<u8>>,
}

#[derive(Debug, Default)]
pub struct B2sFile {
    /// the file this was read from, for the message of a read error
    #[allow(dead_code)]
    pub path: PathBuf,
    pub root: String,
    pub game_name: Option<String>,
    pub grill_height: Option<f64>,
    pub dmd_type: Option<i64>,
    pub dual_backglass: Option<i64>,
    pub dmd: Option<DmdTag>,
    /// every `<DMD>` element of `<Displays>`, in document order. The files in
    /// the wild carry one Target="Backglass" tag; a FullDMD backglass carries
    /// a Target="ScoreView" one as well, with the same rectangle.
    pub dmds: Vec<DmdTag>,
    pub backglass_image: Option<ImageInfo>,
    pub dmd_image: Option<ImageInfo>,
    /// every `<Images>` child, in document order, for the extract command
    pub images: Vec<B2sImage>,
}

impl B2sFile {
    /// The decoded payload of the first `<Images>` child with this element
    /// name (`BackglassImage`, `DMDImage`, ...), when `want_images` was set.
    pub fn image_bytes(&self, element: &str) -> Option<&[u8]> {
        self.images
            .iter()
            .find(|i| i.element == element)
            .and_then(|i| i.bytes.as_deref())
    }
}

type Attrs = Vec<(String, String)>;

fn attr<'a>(attrs: &'a Attrs, key: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

fn parse_f64(s: &str) -> Option<f64> {
    s.trim().parse().ok()
}

fn parse_i64(s: &str) -> Option<i64> {
    s.trim().parse().ok()
}

pub fn probe(path: &Path, want_images: bool) -> Result<B2sFile, String> {
    let file = File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut reader = Reader::from_reader(BufReader::with_capacity(1 << 16, file));
    reader.config_mut().check_end_names = false;
    reader.config_mut().allow_unmatched_ends = true;

    let mut out = B2sFile {
        path: path.to_path_buf(),
        ..Default::default()
    };
    let mut stack: Vec<String> = Vec::new();
    let mut buf = Vec::new();

    loop {
        let event = reader
            .read_event_into(&mut buf)
            .map_err(|e| format!("{}: xml error: {e}", path.display()))?;
        match event {
            Event::Eof => break,
            Event::Start(e) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                handle(&mut out, &stack, &name, &attributes(&e), want_images);
                stack.push(name);
            }
            Event::Empty(e) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                handle(&mut out, &stack, &name, &attributes(&e), want_images);
            }
            Event::End(_) => {
                stack.pop();
            }
            _ => {}
        }
    }
    Ok(out)
}

fn attributes(e: &quick_xml::events::BytesStart) -> Attrs {
    e.attributes()
        .with_checks(false)
        .flatten()
        .map(|a| {
            (
                String::from_utf8_lossy(a.key.as_ref()).into_owned(),
                a.normalized_value(XmlVersion::default())
                    .map(|v| v.into_owned())
                    .unwrap_or_default(),
            )
        })
        .collect()
}

fn handle(out: &mut B2sFile, stack: &[String], name: &str, attrs: &Attrs, want_images: bool) {
    if out.root.is_empty() {
        out.root = name.to_string();
    }
    let in_displays = stack.last().map(|s| s == "Displays").unwrap_or(false);
    match name {
        "GrillHeight" => out.grill_height = attr(attrs, "Value").and_then(parse_f64),
        "GameName" => out.game_name = attr(attrs, "Value").map(String::from),
        "DMDType" => out.dmd_type = attr(attrs, "Value").and_then(parse_i64),
        "DualBackglass" => out.dual_backglass = attr(attrs, "Value").and_then(parse_i64),
        "DMD" if in_displays => {
            let number = |key: &str| attr(attrs, key).and_then(parse_f64).unwrap_or(0.0);
            let tag = DmdTag {
                target: attr(attrs, "Target").unwrap_or("Backglass").to_string(),
                rect: Rect::new(number("X"), number("Y"), number("W"), number("H")),
                glass_roughness: attr(attrs, "GlassRoughness").and_then(parse_f64),
            };
            // the Backglass tag is "the" tag of the older files, the compat
            // field keeps pointing at it; dmds carries all of them
            if tag.target == "Backglass" && out.dmd.is_none() {
                out.dmd = Some(tag.clone());
            }
            out.dmds.push(tag);
        }
        _ => {}
    }
    // every image element inside <Images>: the payloads are base64 in the
    // `Value` attribute. Decoded once; the dedicated backglass/dmd fields
    // take the bytes out of the entry so nothing is decoded twice.
    if stack.last().map(|s| s == "Images").unwrap_or(false)
        && let Some(value) = attr(attrs, "Value")
    {
        let mut img = B2sImage {
            element: name.to_string(),
            source_file_name: attr(attrs, "FileName").map(String::from),
            base64_len: value.len(),
            size: None,
            bytes: None,
        };
        if let Ok(bytes) = decode_image(value) {
            img.size = image_size(&bytes);
            if want_images {
                img.bytes = Some(bytes);
            }
        }
        let info = ImageInfo {
            source_file_name: img.source_file_name.clone(),
            size: img.size,
            base64_len: img.base64_len,
        };
        match name {
            "BackglassImage" => out.backglass_image = Some(info),
            "DMDImage" => out.dmd_image = Some(info),
            _ => {}
        }
        out.images.push(img);
    }
}

/// Decodes the base64 image payload. The XML embeds CRLF sequences inside the
/// base64 text, which are stripped here.
fn decode_image(value: &str) -> Result<Vec<u8>, String> {
    if value.is_empty() {
        return Err("empty".to_string());
    }
    let prefix = ";base64,";
    let b64 = match value.find(prefix) {
        Some(i) => &value[i + prefix.len()..],
        None => value,
    };
    let cleaned: String = if b64.contains(['\r', '\n', ' ', '\t'] as [char; 4]) {
        b64.chars().filter(|c| !c.is_ascii_whitespace()).collect()
    } else {
        b64.to_string()
    };
    base64::engine::general_purpose::STANDARD
        .decode(cleaned.as_bytes())
        .map_err(|e| format!("base64: {e}"))
}

/// Reads the image dimensions from the header without decoding the pixels.
pub fn image_size(bytes: &[u8]) -> Option<Size> {
    if bytes.len() >= 24 && bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        let w = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
        let h = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
        return Some(Size { w, h });
    }
    if bytes.len() >= 10 && bytes.starts_with(b"GIF8") {
        let w = u16::from_le_bytes([bytes[6], bytes[7]]) as u32;
        let h = u16::from_le_bytes([bytes[8], bytes[9]]) as u32;
        return Some(Size { w, h });
    }
    if bytes.len() >= 26 && bytes.starts_with(b"BM") {
        let w = i32::from_le_bytes([bytes[18], bytes[19], bytes[20], bytes[21]]) as u32;
        // a BMP height is signed: negative means top-down rows, the magnitude
        // is the height, and a negative width would be a corrupt header
        let height = i32::from_le_bytes([bytes[22], bytes[23], bytes[24], bytes[25]]);
        let h = u32::try_from(height.abs()).unwrap_or(0);
        return Some(Size { w, h });
    }
    if bytes.len() >= 4 && bytes.starts_with(&[0xFF, 0xD8]) {
        let mut p = 2usize;
        while p + 9 < bytes.len() {
            if bytes[p] != 0xFF {
                break;
            }
            let marker = bytes[p + 1];
            if matches!(marker, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB) {
                let h = u16::from_be_bytes([bytes[p + 5], bytes[p + 6]]) as u32;
                let w = u16::from_be_bytes([bytes[p + 7], bytes[p + 8]]) as u32;
                return Some(Size { w, h });
            }
            if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD9).contains(&marker) {
                p += 2;
                continue;
            }
            let len = u16::from_be_bytes([bytes[p + 2], bytes[p + 3]]) as usize;
            p += 2 + len;
        }
    }
    None
}

#[cfg(test)]
mod decode_tests {
    use super::decode_image;
    #[test]
    fn payload_survives_the_attribute_roundtrip() {
        let xml = "<DirectB2SData><Images><BackglassImage Value=\"QUJD\r\nRA==\"/></Images></DirectB2SData>";
        let path = std::env::temp_dir().join("vpxtool-xml-roundtrip.directb2s");
        std::fs::write(&path, xml).unwrap();
        let f = crate::b2s::xml::probe(&path, true).unwrap();
        assert_eq!(f.images.len(), 1);
        assert_eq!(f.images[0].bytes.as_deref(), Some(&b"ABCD"[..]));
    }

    #[test]
    fn crlf_inside_the_payload_is_stripped() {
        let ok = decode_image("QUJD\r\nRA=="); // "ABCD"
        assert_eq!(ok.as_deref(), Ok(&b"ABCD"[..]));
    }
}
