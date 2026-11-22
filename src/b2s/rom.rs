//! ROM name (`cGameName`) extraction from a table's script.
//!
//! Visual Pinball prefers a `.vbs` sidecar over the script embedded in the
//! `.vpx`, so the same precedence is used here.

use std::fs;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

static CGAMENAME_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)\bcgamename\s*=\s*"([^"\\]*)""#).unwrap());
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
/// assignment found in code. Apostrophe comments and `Rem` statements are
/// stripped, and a name inside a string literal is not an assignment.
pub fn extract_game_name(code: &str) -> Option<String> {
    let unified = code.replace("\r\n", "\n").replace('\r', "\n");
    for line in unified.lines() {
        let code = code_part(line).trim();
        if code.is_empty() {
            continue;
        }
        let lower = code.to_ascii_lowercase();
        if !lower.contains("cgamename") && !lower.contains(".gamename") {
            continue;
        }
        let hit = CGAMENAME_RE
            .captures(code)
            .or_else(|| GAMENAME_RE.captures(code));
        let Some(caps) = hit else { continue };
        let Some(whole) = caps.get(0) else { continue };
        if starts_inside_string(code, whole.start()) {
            continue;
        }
        if let Some(m) = caps.get(1) {
            let name = m.as_str().trim();
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    None
}

/// The code part of a VBScript line: everything before a `'` comment that is
/// not inside a string, and before a `Rem` statement.
///
/// The scanner walks the bytes because `'` and `"` are ASCII: a name like
/// `"it's"` does not start a comment, and `B2ScGameName`/`myGameName` are not
/// `cGameName` assignments. `Rem` only starts a comment at the start of a
/// statement (line start or after `:`).
fn code_part(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut in_string = false;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => in_string = !in_string,
            b'\'' if !in_string => return &line[..i],
            b'r' | b'R' if !in_string && statement_starts(bytes, i) && is_rem(&line[i..]) => {
                return &line[..i];
            }
            _ => {}
        }
        i += 1;
    }
    line
}

fn statement_starts(bytes: &[u8], i: usize) -> bool {
    let mut j = i;
    while j > 0 && (bytes[j - 1] == b' ' || bytes[j - 1] == b'\t') {
        j -= 1;
    }
    j == 0 || bytes[j - 1] == b':'
}

fn is_rem(rest: &str) -> bool {
    let b = rest.as_bytes();
    b.len() >= 3
        && b[..3].eq_ignore_ascii_case(b"rem")
        && (b.len() == 3 || matches!(b[3], b' ' | b'\t' | b'\'' | b':'))
}

/// Is `pos` inside a `"..."` literal of this line? VBScript escapes a quote by
/// doubling it, which toggles twice and leaves the state unchanged.
fn starts_inside_string(line: &str, pos: usize) -> bool {
    let mut in_string = false;
    for b in line.as_bytes().iter().take(pos) {
        if *b == b'"' {
            in_string = !in_string;
        }
    }
    in_string
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
    fn rem_comments_are_skipped() {
        let code =
            "Rem cGameName = \"old\"\nREM CGameName = \"older\"\nConst cGameName = \"real\"\n";
        assert_eq!(extract_game_name(code).as_deref(), Some("real"));
    }

    #[test]
    fn inline_apostrophe_comment_is_not_an_assignment() {
        let code = "debug = 1 ' cGameName = \"old\"\nConst cGameName = \"real\"\n";
        assert_eq!(extract_game_name(code).as_deref(), Some("real"));
    }

    #[test]
    fn rem_after_a_colon_is_a_comment() {
        let code = "x = 1 : Rem cGameName = \"old\"\nConst cGameName = \"real\"\n";
        assert_eq!(extract_game_name(code).as_deref(), Some("real"));
    }

    #[test]
    fn apostrophe_inside_a_string_is_not_a_comment() {
        let code = "s = \"it's fine\"\nConst cGameName = \"real\"\n";
        assert_eq!(extract_game_name(code).as_deref(), Some("real"));
    }

    #[test]
    fn doubled_quotes_do_not_hide_the_assignment() {
        let code = "s = \"a\"\"b\" : Const cGameName = \"real\"\n";
        assert_eq!(extract_game_name(code).as_deref(), Some("real"));
    }

    #[test]
    fn b2s_game_name_is_not_the_rom() {
        let b2s_first = "Const B2ScGameName = \"gameofthrones\"\nConst cGameName = \"got_137h\"\n";
        assert_eq!(extract_game_name(b2s_first).as_deref(), Some("got_137h"));
        let rom_first = "Const cGameName = \"got_137h\"\nConst B2ScGameName = \"gameofthrones\"\n";
        assert_eq!(extract_game_name(rom_first).as_deref(), Some("got_137h"));
    }

    #[test]
    fn a_lone_b2s_game_name_is_not_a_rom() {
        let code = "Const B2ScGameName = \"gameofthrones\"\n";
        assert_eq!(extract_game_name(code), None);
    }

    #[test]
    fn gamename_inside_a_string_is_not_an_assignment() {
        let code = "MsgBox \"cGameName = \"\"fake\"\"\"\nConst cGameName = \"real\"\n";
        assert_eq!(extract_game_name(code).as_deref(), Some("real"));
    }

    #[test]
    fn lone_cr_line_endings() {
        let code = "Dim x\rConst cGameName = \"acd_170h\"\r";
        assert_eq!(extract_game_name(code).as_deref(), Some("acd_170h"));
    }
}
