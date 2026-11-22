//! Extract the images stored base64-encoded in a `.directb2s` / `.b2spro` file.
//!
//! The payloads sit in the `Value` attribute of the `<Images>` children
//! (BackglassImage, DMDImage, ThumbnailImage, ...), 5-19 MB of base64 each,
//! with CRLF sequences embedded in the text. [`xml::probe`] decodes them;
//! here each decoded payload is written next to the others with an extension
//! taken from the magic bytes, because the `FileName` attribute is the
//! designer's source path and lies about the format of the embedded image.

use std::fs;
use std::path::Path;

use crate::b2s::xml;

/// One image written out by [`extract`].
#[derive(Debug)]
pub struct ExtractedFile {
    /// the element the payload came from, e.g. `BackglassImage`
    pub element: String,
    pub path: std::path::PathBuf,
    pub bytes: usize,
}

/// Images whose payload could not be decoded (empty or corrupt base64),
/// reported as notes rather than failing the whole extract.
#[derive(Debug)]
pub struct SkippedImage {
    pub element: String,
    pub reason: String,
}

#[derive(Debug, Default)]
pub struct ExtractOutcome {
    pub written: Vec<ExtractedFile>,
    pub skipped: Vec<SkippedImage>,
}

/// Extracts every image of the file into `output_dir`, creating it when
/// missing. Files are named `<element-lowercase>.<real-format>`; a repeated
/// element (dual backglass thumbnails) gets a numeric suffix.
pub fn extract(path: &Path, output_dir: &Path) -> Result<ExtractOutcome, String> {
    let b2s = xml::probe(path, true)?;
    fs::create_dir_all(output_dir).map_err(|e| format!("create {}: {e}", output_dir.display()))?;

    let mut outcome = ExtractOutcome::default();
    let mut used: Vec<String> = Vec::new();
    for img in &b2s.images {
        let Some(bytes) = &img.bytes else {
            outcome.skipped.push(SkippedImage {
                element: img.element.clone(),
                reason: if img.base64_len == 0 {
                    "empty payload".into()
                } else {
                    "payload is not valid base64".into()
                },
            });
            continue;
        };
        let base = img.element.to_ascii_lowercase();
        let ext = image_extension(bytes);
        let mut name = format!("{base}.{ext}");
        let mut n = 1;
        while used.contains(&name) {
            name = format!("{base}-{}.{ext}", {
                n += 1;
                n - 1
            });
        }
        used.push(name.clone());
        let file_path = output_dir.join(&name);
        fs::write(&file_path, bytes).map_err(|e| format!("write {}: {e}", file_path.display()))?;
        outcome.written.push(ExtractedFile {
            element: img.element.clone(),
            path: file_path,
            bytes: bytes.len(),
        });
    }
    Ok(outcome)
}

/// One image written back into the file by [`import`].
#[derive(Debug)]
pub struct ImportedFile {
    /// the element whose payload was replaced, as it is spelled in the file
    pub element: String,
    /// the image file it was taken from
    pub source: std::path::PathBuf,
    pub bytes: usize,
}

#[derive(Debug, Default)]
pub struct ImportOutcome {
    pub replaced: Vec<ImportedFile>,
    /// per-file notes: an image of the folder with no matching element
    pub notes: Vec<String>,
}

/// Imports the images of `dir` back into the `.directb2s`/`.b2spro` file,
/// the counterpart of [`extract`]: every file of the folder whose stem is an
/// element name (`backglassimage.png`, `dmdimage.png`, ...) replaces the
/// base64 payload of that element. The element names are matched
/// case-insensitively, a `-N` suffix addresses the Nth element of a repeated
/// name (the form [`extract`] writes for dual backglass thumbnails). Only
/// the payload between the `Value="` quotes is rewritten; the rest of the
/// file, images not in the folder included, stays byte-identical. The
/// payload is written the way the B2S editors write it: base64 in 76-char
/// lines, the line ends as the `&#xD;&#xA;` entity.
pub fn import(path: &Path, dir: &Path, backup: bool) -> Result<ImportOutcome, String> {
    let bytes = fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    if !contains(b"</DirectB2SData>", &bytes) {
        return Err(format!("{}: not a B2S file", path.display()));
    }
    // the search runs on a lowercased copy, the splice on the original
    let mut lower = bytes.clone();
    lower.make_ascii_lowercase();
    let images_start = find(&lower, b"<images", 0, lower.len()).ok_or("no <Images> block")?;
    let images_end =
        find(&lower, b"</images>", images_start, lower.len()).ok_or("no </Images> end tag")?;

    let mut entries: Vec<(String, usize, std::path::PathBuf)> = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| format!("read {}: {e}", dir.display()))? {
        let path = entry
            .map_err(|e| format!("read {}: {e}", dir.display()))?
            .path();
        if !path.is_file() {
            continue;
        }
        let Some(stem) = path.file_stem().map(|s| s.to_string_lossy().to_lowercase()) else {
            continue;
        };
        // `base-2` is the second element of a repeated name
        let (base, index) = match stem.rsplit_once('-') {
            Some((base, n)) if n.parse::<usize>().is_ok_and(|n| n >= 2) => {
                (base.to_string(), n.parse::<usize>().unwrap())
            }
            _ => (stem, 1),
        };
        if base.is_empty() {
            continue;
        }
        entries.push((base, index, path));
    }
    entries.sort_by(|a, b| a.2.cmp(&b.2));

    let mut outcome = ImportOutcome::default();
    // (payload start, payload end, new payload) - collected first, applied
    // from the end so the earlier spans stay valid
    let mut spans: Vec<(usize, usize, Vec<u8>, String, std::path::PathBuf)> = Vec::new();
    for (base, index, file) in entries {
        let tag = format!("<{base}");
        let Some(pos) = nth_element(&lower, tag.as_bytes(), images_start, images_end, index) else {
            outcome.notes.push(format!(
                "{}: no {base} element in {}, left alone",
                file.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                path.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string())
            ));
            continue;
        };
        // ` value="` comes within the element's attributes, before the
        // payload; the tag's end is megabytes further, so bound the search
        // to the attribute area, not to a `>`
        let Some(value) = find(&lower, b" value=\"", pos, pos + 65536) else {
            outcome.notes.push(format!(
                "{base}: element has no Value attribute, left alone"
            ));
            continue;
        };
        let payload_start = value + b" value=\"".len();
        // the payload is base64, entities and whitespace only - no quotes
        let payload_end = find(&lower, b"\"", payload_start, bytes.len())
            .ok_or("unterminated Value attribute")?;
        let element = String::from_utf8_lossy(
            &bytes[pos + 1..find(&bytes, b" ", pos, pos + 65536).unwrap_or(pos + 1)],
        )
        .to_string();
        let image = fs::read(&file).map_err(|e| format!("read {}: {e}", file.display()))?;
        spans.push((
            payload_start,
            payload_end,
            encode_wrapped(&image),
            element,
            file,
        ));
    }
    spans.sort_by_key(|s| std::cmp::Reverse(s.0));
    let mut out = bytes.clone();
    for (start, end, payload, _, _) in &spans {
        out.splice(*start..*end, payload.iter().copied());
    }

    if backup {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let bak = path.with_file_name(format!("{name}.bak"));
        // the oldest state this tool has seen is kept, as in the stamp pass
        if !bak.exists() {
            fs::copy(path, &bak).map_err(|e| format!("backup {}: {e}", bak.display()))?;
        }
    }
    // write beside the original, then swap, so an interrupt cannot leave a
    // half-written backglass
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, &out).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    fs::rename(&tmp, path).map_err(|e| format!("replace {}: {e}", path.display()))?;

    // report in file order, not splice order
    spans.sort_by(|a, b| a.4.cmp(&b.4));
    for (_, _, payload, element, file) in spans {
        outcome.replaced.push(ImportedFile {
            element,
            source: file,
            bytes: payload.len(),
        });
    }
    Ok(outcome)
}

/// The payload form of the B2S editors: standard base64 in 76-char lines,
/// the CRLF written as the `&#xD;&#xA;` entity.
fn encode_wrapped(bytes: &[u8]) -> Vec<u8> {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    let mut out = Vec::with_capacity(b64.len() + b64.len() / 76 * 10);
    for (i, chunk) in b64.as_bytes().chunks(76).enumerate() {
        if i > 0 {
            out.extend_from_slice(b"&#xD;&#xA;");
        }
        out.extend_from_slice(chunk);
    }
    out
}

/// The position of the `index`th (1-based) element tag `<name ...` inside
/// `[from, to)`, the name matched on the lowercased copy. The next byte
/// must end the name: `dmdimage` must not match `<dmdimagex`.
fn nth_element(haystack: &[u8], tag: &[u8], from: usize, to: usize, index: usize) -> Option<usize> {
    let mut pos = from;
    let mut seen = 0;
    loop {
        let p = find(haystack, tag, pos, to)?;
        let next = haystack.get(p + tag.len())?;
        if matches!(next, b' ' | b'\t' | b'\r' | b'\n' | b'>' | b'/') {
            seen += 1;
            if seen == index {
                return Some(p);
            }
        }
        pos = p + tag.len();
    }
}

fn find(haystack: &[u8], needle: &[u8], from: usize, to: usize) -> Option<usize> {
    let to = to.min(haystack.len());
    if from + needle.len() > to {
        return None;
    }
    haystack[from..to]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

fn contains(needle: &[u8], haystack: &[u8]) -> bool {
    find(haystack, needle, 0, haystack.len()).is_some()
}

/// The file extension for the magic bytes of a decoded payload. The B2S
/// payloads are PNG, JPEG, BMP or GIF; undecodable payloads keep `.img`,
/// the convention of the B2S editors for a raw dump.
fn image_extension(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        "png"
    } else if bytes.starts_with(&[0xFF, 0xD8]) {
        "jpg"
    } else if bytes.starts_with(b"BM") {
        "bmp"
    } else if bytes.starts_with(b"GIF8") {
        "gif"
    } else {
        "img"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_come_from_the_magic_bytes() {
        assert_eq!(image_extension(&[0x89, b'P', b'N', b'G', 0, 0]), "png");
        assert_eq!(image_extension(&[0xFF, 0xD8, 0xFF, 0xE0]), "jpg");
        assert_eq!(image_extension(b"BMab"), "bmp");
        assert_eq!(image_extension(b"GIF89a"), "gif");
        assert_eq!(image_extension(b"nonsense"), "img");
    }

    fn fixture(name: &str, xml: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("vpxtool-b2s-extract-tests");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        fs::write(&path, xml).unwrap();
        path
    }

    fn tiny_png() -> Vec<u8> {
        vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ]
    }

    #[test]
    fn extract_then_import_restores_the_file_byte_for_byte() {
        // a payload in the form the B2S editors write: base64, 76-char lines,
        // the CRLF as the &#xD;&#xA; entity
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(tiny_png());
        let wrapped = b64.as_bytes().chunks(76).collect::<Vec<_>>();
        let wrapped = wrapped
            .iter()
            .map(|c| String::from_utf8_lossy(c).into_owned())
            .collect::<Vec<_>>()
            .join("&#xD;&#xA;");
        let xml = format!(
            "<DirectB2SData Version=\"1.0\"><Images>\r\n<BackglassImage Value=\"{wrapped}\" FileName=\"a.png\"/>\r\n<DMDImage Value=\"\"/>\r\n</Images></DirectB2SData>"
        );
        let path = fixture("roundtrip.directb2s", &xml);
        let original = fs::read(&path).unwrap();
        let out = std::env::temp_dir().join("vpxtool-b2s-roundtrip.out");
        let _ = fs::remove_dir_all(&out);
        let outcome = extract(&path, &out).unwrap();
        assert_eq!(
            outcome.written.len(),
            1,
            "the empty DMD is skipped: {outcome:?}"
        );
        // the import puts the folder back, the file must not change
        let imported = import(&path, &out, false).unwrap();
        assert_eq!(imported.replaced.len(), 1);
        assert_eq!(imported.replaced[0].element, "BackglassImage");
        assert_eq!(fs::read(&path).unwrap(), original);
    }

    #[test]
    fn import_replaces_one_payload_and_keeps_the_rest() {
        use base64::Engine;
        let std = base64::engine::general_purpose::STANDARD;
        let old = std.encode(b"old image");
        let xml = format!(
            "<DirectB2SData><Images><BackglassImage Value=\"{old}\"/><DMDImage Value=\"{}\"/></Images></DirectB2SData>",
            std.encode(b"dmd")
        );
        let path = fixture("replace.directb2s", &xml);
        let dir = std::env::temp_dir().join("vpxtool-b2s-replace.in");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("backglassimage.png"), b"new image").unwrap();
        fs::write(dir.join("thumbnailimage.png"), b"no element").unwrap();
        let outcome = import(&path, &dir, false).unwrap();
        assert_eq!(outcome.replaced.len(), 1);
        let after = fs::read_to_string(&path).unwrap();
        assert!(after.contains(&format!("Value=\"{}\"", std.encode(b"new image"))));
        // payloads of images of the folder without an element are reported
        // and the other payloads stay as they were
        assert!(after.contains(&format!("Value=\"{}\"", std.encode(b"dmd"))));
        assert_eq!(outcome.notes.len(), 1);
        assert!(outcome.notes[0].contains("thumbnailimage"));
    }

    #[test]
    fn writes_every_image_with_the_real_extension() {
        // a 1x1 PNG payload, base64'd, with an embedded CRLF like real files
        let png = tiny_png();
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &png);
        let b64 = format!("{}{}\r\n{}", &b64[..20], &b64[20..40], &b64[40..]);
        let xml = format!(
            "<DirectB2SData Version=\"1.0\"><Images>\
             <BackglassImage Value=\"{b64}\"/>\
             <ThumbnailImage Value=\"{b64}\"/>\
             <DMDImage Value=\"\"/>\
             </Images></DirectB2SData>"
        );
        let path = fixture("images.directb2s", &xml);
        let out = std::env::temp_dir().join("vpxtool-b2s-extract-tests.out");
        let _ = fs::remove_dir_all(&out);
        let _ = fs::remove_file(&out);
        let outcome = extract(&path, &out).unwrap();
        assert_eq!(outcome.written.len(), 2);
        assert_eq!(outcome.skipped.len(), 1);
        assert_eq!(outcome.skipped[0].element, "DMDImage");
        for f in &outcome.written {
            assert!(f.path.extension().unwrap() == "png");
            assert_eq!(fs::read(&f.path).unwrap(), png);
        }
        assert!(out.join("backglassimage.png").exists());
        assert!(out.join("thumbnailimage.png").exists());
    }
}
