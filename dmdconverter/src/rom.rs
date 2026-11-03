//! ROM name (`cGameName`) extraction from a table's script.
//!
//! Visual Pinball prefers a `.vbs` sidecar over the script embedded in the
//! `.vpx`, so the same precedence is used here.

use std::fs;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

static CGAMENAME_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)cgamename\s*=\s*"([^"\\]*)""#).unwrap());
static GAMENAME_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)\.gamename\s*=\s*"([^"\\]*)""#).unwrap());

/// The script of a table: sidecar `.vbs` if present, else the embedded script.
pub fn script_for_vpx(vpx: &Path) -> Result<String, String> {
    let sidecar = vpx.with_extension("vbs");
    if sidecar.exists() {
        let bytes = fs::read(&sidecar).map_err(|e| format!("read {}: {e}", sidecar.display()))?;
        return Ok(decode_script(&bytes));
    }
    let mut file = vpin::vpx::open(vpx).map_err(|e| format!("open {}: {e}", vpx.display()))?;
    let game_data = file
        .read_gamedata()
        .map_err(|e| format!("{}: read gamedata: {e}", vpx.display()))?;
    Ok(game_data.code.string)
}

/// Scripts are often cp1252 (authors write names like `AC-DC`), not UTF-8.
fn decode_script(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => encoding_rs::WINDOWS_1252.decode(bytes).0.into_owned(),
    }
}

/// Returns the first `cGameName = "..."` (or FlexDMD `.GameName = "..."`)
/// assignment found in non-comment lines.
pub fn extract_game_name(code: &str) -> Option<String> {
    let unified = code.replace("\r\n", "\n").replace('\r', "\n");
    for line in unified.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('\'') {
            continue;
        }
        let lower = trimmed.to_lowercase();
        if !lower.contains("cgamename") && !lower.contains(".gamename") {
            continue;
        }
        if let Some(caps) = CGAMENAME_RE
            .captures(trimmed)
            .or_else(|| GAMENAME_RE.captures(trimmed))
        {
            if let Some(m) = caps.get(1) {
                let name = m.as_str().trim();
                if !name.is_empty() {
                    return Some(name.to_string());
                }
            }
        }
    }
    None
}

pub fn game_name_from_vpx(vpx: &Path) -> Result<Option<String>, String> {
    let code = script_for_vpx(vpx)?;
    Ok(extract_game_name(&code))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn const_cgamename() {
        let code = "Dim cGameName\r\nConst cGameName = \"afm_113b\" 'arcade rom\r\n";
        assert_eq!(extract_game_name(code).as_deref(), Some("afm_113b"));
    }

    #[test]
    fn flexdmd_gamename() {
        let code = "FlexDMD.GameName = \"tom_10h\"\n";
        assert_eq!(extract_game_name(code).as_deref(), Some("tom_10h"));
    }

    #[test]
    fn skips_commented_lines() {
        let code = "' Const cGameName = \"ignored\"\nConst cGameName = \"real\"\n";
        assert_eq!(extract_game_name(code).as_deref(), Some("real"));
    }

    #[test]
    fn lone_cr_line_endings() {
        let code = "Dim x\rConst cGameName = \"acd_170h\"\r";
        assert_eq!(extract_game_name(code).as_deref(), Some("acd_170h"));
    }
}
