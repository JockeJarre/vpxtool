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

#[derive(Default)]
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

    #[test]
    fn writes_every_image_with_the_real_extension() {
        // a 1x1 PNG payload, base64'd, with an embedded CRLF like real files
        let png = [
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, png);
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
