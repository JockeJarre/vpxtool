//! Writes the `<DMD>` element into a B2S XML file, in place, keeping the rest
//! of the file byte-identical.
//!
//! These files are 25-53 MB because of the base64 images, so the edit is
//! targeted: locate the element in the tail of the file, splice the new text
//! in, copy everything else through unchanged. `<Displays>` is the last child
//! of the root element in the files B2S-Pro writes, so a missing block is
//! inserted directly before the root end tag.
//!
//! The structural tail of these files is small, so the line ending used there
//! is detected and the inserted text follows it; the sample files use LF.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::scan::{MatchKind, Record};
use crate::Rect;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Action {
    /// the file already holds this rectangle
    Unchanged,
    /// rewrite the attributes of the existing `<DMD>` element
    Replace,
    /// add a `<Displays>` block
    Insert,
}

impl std::fmt::Display for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Action::Unchanged => "unchanged",
            Action::Replace => "replace",
            Action::Insert => "insert",
        };
        f.write_str(s)
    }
}

#[derive(Debug)]
pub struct Plan {
    pub path: PathBuf,
    pub action: Action,
    /// byte span that `payload` replaces; an empty span is a pure insertion
    pub span: (usize, usize),
    pub payload: String,
    /// the rectangle currently in the file
    pub from: Option<Rect>,
    pub to: Rect,
}

/// The `<DMD>` element text B2S-Pro writes for this rectangle.
pub fn tag_text(rect: Rect, glass_roughness: f64) -> String {
    format!(
        "<DMD Target=\"Backglass\" X=\"{:.0}\" Y=\"{:.0}\" W=\"{:.0}\" H=\"{:.0}\" GlassRoughness=\"{}\"/>",
        rect.x, rect.y, rect.w, rect.h, fmt_glass(glass_roughness)
    )
}

/// 0.1 -> "0.1", 1.0 -> "1.0" (the form B2S-Pro writes for whole values).
fn fmt_glass(v: f64) -> String {
    if v == v.trunc() {
        format!("{v:.1}")
    } else {
        format!("{v}")
    }
}

/// Decides what has to change, from the content of `path`.
pub fn plan_bytes(
    path: &Path,
    bytes: &[u8],
    to: Rect,
    current: Option<Rect>,
    glass_roughness: f64,
) -> Result<Plan, String> {
    let root_end = rfind(bytes, b"</DirectB2SData>").ok_or("no </DirectB2SData> end tag")?;
    // the image payloads sit above the structural tail, so only search below
    // the end of <Images>
    let tail = rfind(bytes, b"</Images>")
        .map(|p| p + b"</Images>".len())
        .unwrap_or(0)
        .min(root_end);
    let le = line_ending(bytes, tail);
    let tag = tag_text(to, glass_roughness);
    // compare the way the tag is written (`{:.0}`), not the raw floats: the
    // scan keeps the fractional rectangle, while the file holds whole pixels,
    // so a raw compare would rewrite identical bytes on every run
    let to_text = to.to_string();
    // <Displays> is a child of the root, so it goes one level below the root
    // end tag: the shape B2S-Pro writes
    let base = format!("{}    ", indent_of(bytes, root_end));
    let block = format!("{base}<Displays>{le}{base}    {tag}{le}{base}</Displays>{le}");
    let mut plan = Plan {
        path: path.to_path_buf(),
        action: Action::Insert,
        span: (line_start(bytes, root_end), line_start(bytes, root_end)),
        payload: block,
        from: current,
        to,
    };
    if let Some(start) = find_from(bytes, b"<Displays", tail).filter(|p| *p < root_end) {
        if let Some(span) = dmd_tag_span(bytes, start, root_end) {
            plan.span = span;
            plan.action = if current.map(|r| r.to_string()) == Some(to_text) {
                Action::Unchanged
            } else {
                Action::Replace
            };
            plan.payload = if plan.action == Action::Unchanged {
                String::new()
            } else {
                tag.clone()
            };
            return Ok(plan);
        }
        // a <Displays> element with no <DMD>: replace a self-closing one, or
        // add the tag line inside an open one
        if let Some(gt) = find_from(bytes, b">", start + 9).filter(|p| *p < root_end) {
            let indent = indent_of(bytes, start);
            let inner = format!("{indent}    ");
            if bytes[start + 9..=gt].contains(&b'/') {
                plan.span = (start, gt + 1);
                plan.payload = format!("{indent}<Displays>{le}{inner}{tag}{le}{indent}</Displays>");
            } else {
                plan.span = (gt + 1, gt + 1);
                plan.payload = format!("{le}{inner}{tag}");
            }
            return Ok(plan);
        }
    }
    Ok(plan)
}

/// Applies the plan and returns the size of the new file. A `.bak` copy of the
/// original is kept when `backup` is true.
pub fn apply(plan: &Plan, bytes: &[u8], backup: bool) -> Result<usize, String> {
    if plan.action == Action::Unchanged {
        return Ok(bytes.len());
    }
    let (from, to) = plan.span;
    let mut out = Vec::with_capacity(bytes.len() + plan.payload.len());
    out.extend_from_slice(&bytes[..from]);
    out.extend_from_slice(plan.payload.as_bytes());
    out.extend_from_slice(&bytes[to..]);
    if backup {
        let name = plan
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let bak = plan.path.with_file_name(format!("{name}.bak"));
        // an existing `.bak` is the oldest state this tool has seen: keep it,
        // so a second run with other options cannot overwrite the original
        if !bak.exists() {
            fs::copy(&plan.path, &bak).map_err(|e| format!("backup {}: {e}", bak.display()))?;
        }
    }
    // write beside the original, then swap, so an interrupt cannot leave a
    // half-written backglass
    let tmp = plan.path.with_extension("tmp");
    fs::write(&tmp, &out).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    fs::rename(&tmp, &plan.path).map_err(|e| format!("replace {}: {e}", plan.path.display()))?;
    Ok(out.len())
}

#[derive(Debug, Default)]
pub struct Stats {
    pub seen: usize,
    pub unchanged: usize,
    pub replaced: usize,
    pub inserted: usize,
    pub written: usize,
    pub skipped: usize,
    pub duplicate: usize,
    pub failed: usize,
}

/// Plans (and with `write`, applies) the stamp for every record that has a
/// rectangle. A plan that fails does not stop the run.
pub fn stamp(records: &[Record], glass_roughness: f64, write: bool, backup: bool) -> Stats {
    let mut stats = Stats::default();
    // several tables can match the same backglass file; stamp each file once,
    // and let an exact name match win over a similar one
    let mut owner: HashMap<&Path, usize> = HashMap::new();
    for (i, rec) in records.iter().enumerate() {
        let (Some(path), Some(_)) = (rec.b2s_path.as_deref(), rec.chosen) else {
            stats.skipped += 1;
            continue;
        };
        match owner.get_mut(path) {
            Some(j) => {
                stats.duplicate += 1;
                if rec.match_kind == MatchKind::Exact && records[*j].match_kind != MatchKind::Exact
                {
                    *j = i;
                }
            }
            None => {
                owner.insert(path, i);
            }
        }
    }
    for (i, rec) in records.iter().enumerate() {
        let (Some(path), Some(rect)) = (rec.b2s_path.as_deref(), rec.chosen) else {
            continue;
        };
        if owner.get(path) != Some(&i) {
            continue;
        }
        stats.seen += 1;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) => {
                stats.failed += 1;
                eprintln!("  error   {name}: read: {e}");
                continue;
            }
        };
        match plan_bytes(path, &bytes, rect, rec.existing_dmd, glass_roughness) {
            Ok(plan) => {
                match plan.action {
                    Action::Unchanged => stats.unchanged += 1,
                    Action::Replace => stats.replaced += 1,
                    Action::Insert => stats.inserted += 1,
                }
                println!(
                    "  {:<9} {}  {} -> {}",
                    plan.action,
                    name,
                    plan.from
                        .map(|r| r.to_string())
                        .unwrap_or_else(|| "-".into()),
                    plan.to
                );
                if write && plan.action != Action::Unchanged {
                    match apply(&plan, &bytes, backup) {
                        Ok(_) => stats.written += 1,
                        Err(e) => {
                            stats.failed += 1;
                            eprintln!("  error   {name}: {e}");
                        }
                    }
                }
            }
            Err(e) => {
                stats.failed += 1;
                eprintln!("  error   {name}: {e}");
            }
        }
    }
    stats
}

pub fn print_stats(stats: &Stats, write: bool) {
    let verb = if write { "written" } else { "to write" };
    println!("\n{}", if write { "stamped" } else { "dry run" });
    println!("  with a rect       {}", stats.seen);
    println!("  unchanged         {}", stats.unchanged);
    println!("  replace tag       {}", stats.replaced);
    println!("  insert block      {}", stats.inserted);
    println!("  {verb:<18}{}", stats.written);
    if stats.skipped > 0 {
        println!("  no rect / no b2s  {}", stats.skipped);
    }
    if stats.duplicate > 0 {
        println!("  same b2s file     {}", stats.duplicate);
    }
    if stats.failed > 0 {
        println!("  failed            {}", stats.failed);
    }
}

/// `<DMD ... />` inside `[from, to)` as a byte span. Attribute values cannot
/// contain `<` or `>`, so the first `>` after the tag name ends the element.
fn dmd_tag_span(bytes: &[u8], from: usize, to: usize) -> Option<(usize, usize)> {
    let start = find_from(bytes, b"<DMD ", from)?;
    if start >= to {
        return None;
    }
    let end = bytes[start + 5..to].iter().position(|&c| c == b'>')? + start + 6;
    Some((start, end))
}

/// The first byte of the line that holds `at`.
fn line_start(bytes: &[u8], at: usize) -> usize {
    match bytes[..at].iter().rposition(|&c| c == b'\n') {
        Some(p) => p + 1,
        None => at,
    }
}

/// The indentation of the line that holds `at`.
fn indent_of(bytes: &[u8], at: usize) -> String {
    let start = line_start(bytes, at);
    bytes[start..at]
        .iter()
        .take_while(|c| **c == b' ' || **c == b'\t')
        .map(|&c| c as char)
        .collect()
}

/// The line ending used by the structural tail, from `from` to the end of the
/// file. The base64 payloads above it contain CRLF inside their attribute
/// values, so the whole file is not a CRLF file.
fn line_ending(bytes: &[u8], from: usize) -> &'static str {
    let tail = &bytes[from.min(bytes.len())..];
    if tail.windows(2).any(|w| w == b"\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

fn find_from(bytes: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from > bytes.len() {
        return None;
    }
    bytes[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

fn rfind(bytes: &[u8], needle: &[u8]) -> Option<usize> {
    bytes.windows(needle.len()).rposition(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WITH_TAG: &[u8] = b"<DirectB2SData Version=\"1.2\">\n    <Images>\n    </Images>\n    <Displays>\n        <DMD Target=\"Backglass\" X=\"484\" Y=\"1240\" W=\"1034\" H=\"218\" GlassRoughness=\"0.1\"/>\n    </Displays>\n</DirectB2SData>\n";
    const NO_DISPLAYS: &[u8] =
        b"<DirectB2SData Version=\"1.2\">\n    <Images>\n    </Images>\n</DirectB2SData>\n";
    const EMPTY_DISPLAYS: &[u8] = b"<DirectB2SData Version=\"1.2\">\n    <Images>\n    </Images>\n    <Displays>\n    </Displays>\n</DirectB2SData>\n";

    fn target() -> Rect {
        Rect::new(10.0, 20.0, 120.0, 30.0)
    }

    /// reads the file, then plans, as the stamp pass does
    fn plan_at(path: &Path, to: Rect, current: Option<Rect>, glass: f64) -> Result<Plan, String> {
        let bytes = fs::read(path).map_err(|e| e.to_string())?;
        plan_bytes(path, &bytes, to, current, glass)
    }

    fn tmp_file(name: &str, bytes: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir().join("dmdconverter-tests");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn rewrites_the_existing_element_only() {
        let path = tmp_file("has.directb2s", WITH_TAG);
        let current = Rect::new(484.0, 1240.0, 1034.0, 218.0);
        let plan = plan_at(&path, target(), Some(current), 0.1).unwrap();
        assert_eq!(plan.action, Action::Replace);
        apply(&plan, &fs::read(&path).unwrap(), true).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(
            text,
            "<DirectB2SData Version=\"1.2\">\n    <Images>\n    </Images>\n    <Displays>\n        <DMD Target=\"Backglass\" X=\"10\" Y=\"20\" W=\"120\" H=\"30\" GlassRoughness=\"0.1\"/>\n    </Displays>\n</DirectB2SData>\n"
        );
        assert_eq!(text.matches("<DMD ").count(), 1);
        assert!(tmp_file("has.directb2s", WITH_TAG)
            .with_file_name("has.directb2s.bak")
            .exists());
    }

    #[test]
    fn appends_a_displays_block_when_missing() {
        let path = tmp_file("missing.directb2s", NO_DISPLAYS);
        let plan = plan_at(&path, target(), None, 0.1).unwrap();
        assert_eq!(plan.action, Action::Insert);
        apply(&plan, &fs::read(&path).unwrap(), false).unwrap();
        let want = String::from_utf8_lossy(NO_DISPLAYS).replace(
            "</DirectB2SData>",
            &(plan.payload.clone() + "</DirectB2SData>"),
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), want);
        assert_eq!(
            fs::read_to_string(&path)
                .unwrap()
                .matches("<Displays")
                .count(),
            1
        );
    }

    #[test]
    fn fills_an_empty_displays_block() {
        let path = tmp_file("empty.directb2s", EMPTY_DISPLAYS);
        let plan = plan_at(&path, target(), None, 0.1).unwrap();
        assert_eq!(plan.action, Action::Insert);
        apply(&plan, &fs::read(&path).unwrap(), false).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "<DirectB2SData Version=\"1.2\">\n    <Images>\n    </Images>\n    <Displays>\n        <DMD Target=\"Backglass\" X=\"10\" Y=\"20\" W=\"120\" H=\"30\" GlassRoughness=\"0.1\"/>\n    </Displays>\n</DirectB2SData>\n"
        );
    }

    #[test]
    fn fills_a_self_closing_displays_element() {
        let path = tmp_file(
            "selfclosing.directb2s",
            b"<DirectB2SData Version=\"1.2\">\n    <Images>\n    </Images>\n    <Displays />\n</DirectB2SData>\n",
        );
        let plan = plan_at(&path, target(), None, 0.1).unwrap();
        apply(&plan, &fs::read(&path).unwrap(), false).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches("<Displays").count(), 1);
        assert_eq!(text.matches("<DMD ").count(), 1);
        assert!(text.ends_with("    </Displays>\n</DirectB2SData>\n"));
    }

    #[test]
    fn an_already_correct_tag_is_left_alone() {
        let path = tmp_file("same.directb2s", WITH_TAG);
        let rect = Rect::new(484.0, 1240.0, 1034.0, 218.0);
        let plan = plan_at(&path, rect, Some(rect), 0.1).unwrap();
        assert_eq!(plan.action, Action::Unchanged);
        apply(&plan, &fs::read(&path).unwrap(), false).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            String::from_utf8_lossy(WITH_TAG)
        );
    }

    #[test]
    fn a_fractional_rect_that_rounds_to_the_tag_is_unchanged() {
        // the writer formats whole pixels, so 10.4 has to compare equal to the
        // 10 that is in the file; otherwise every run rewrites identical bytes
        let path = tmp_file("fractional.directb2s", WITH_TAG);
        let to = Rect::new(484.2, 1240.4, 1034.3, 218.2);
        let current = Rect::new(484.0, 1240.0, 1034.0, 218.0);
        let plan = plan_at(&path, to, Some(current), 0.1).unwrap();
        assert_eq!(plan.action, Action::Unchanged);
        assert!(plan.payload.is_empty());
    }

    #[test]
    fn the_second_run_finds_nothing_to_do() {
        let path = tmp_file("idempotent.directb2s", NO_DISPLAYS);
        let to = Rect::new(10.4, 20.6, 120.6, 30.6);
        let first = plan_at(&path, to, None, 0.1).unwrap();
        assert_eq!(first.action, Action::Insert);
        apply(&first, &fs::read(&path).unwrap(), false).unwrap();
        let stamped = fs::read(&path).unwrap();
        // the scan sees no tag before the first run, and the rounded values
        // after it: the second plan must not rewrite the file
        let second = plan_bytes(
            &path,
            &stamped,
            to,
            Some(Rect::new(10.0, 21.0, 121.0, 31.0)),
            0.1,
        )
        .unwrap();
        assert_eq!(second.action, Action::Unchanged);
        assert_eq!(fs::read(&path).unwrap(), stamped);
    }

    #[test]
    fn an_existing_backup_is_not_overwritten() {
        let path = tmp_file("keepbak.directb2s", WITH_TAG);
        let bak = path.with_file_name("keepbak.directb2s.bak");
        let _ = fs::remove_file(&bak);
        let current = Rect::new(484.0, 1240.0, 1034.0, 218.0);
        let first = plan_at(&path, target(), Some(current), 0.1).unwrap();
        apply(&first, &fs::read(&path).unwrap(), true).unwrap();
        let original = fs::read(&bak).unwrap();
        let second = plan_at(&path, Rect::new(1.0, 2.0, 3.0, 4.0), Some(current), 0.1).unwrap();
        apply(&second, &fs::read(&path).unwrap(), true).unwrap();
        assert_eq!(fs::read(&bak).unwrap(), original);
    }

    #[test]
    fn the_same_backglass_is_stamped_once() {
        let path = tmp_file("twice.directb2s", NO_DISPLAYS);
        let rect = target();
        let first = Record {
            vpx: "a.vpx".into(),
            b2s_path: Some(path.clone()),
            match_kind: MatchKind::Similar,
            chosen: Some(Rect::new(1.0, 1.0, 1.0, 1.0)),
            ..Default::default()
        };
        let second = Record {
            vpx: "b.vpx".into(),
            b2s_path: Some(path.clone()),
            match_kind: MatchKind::Exact,
            chosen: Some(rect),
            ..Default::default()
        };
        let stats = stamp(&[first, second], 0.1, true, false);
        assert_eq!(stats.seen, 1);
        assert_eq!(stats.duplicate, 1);
        assert_eq!(stats.written, 1);
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches("<DMD ").count(), 1);
        // the exact name match wins over the similar one
        assert!(text.contains("X=\"10\" Y=\"20\" W=\"120\" H=\"30\""));
    }

    #[test]
    fn the_inserted_block_has_the_shape_b2s_pro_writes() {
        let path = tmp_file("shape.directb2s", NO_DISPLAYS);
        let plan = plan_at(&path, target(), None, 0.1).unwrap();
        // B2S Pro writes the block at one indent below `</Images>`, the element
        // at two, with the attributes in this order
        assert_eq!(
            plan.payload,
            "    <Displays>\n        <DMD Target=\"Backglass\" X=\"10\" Y=\"20\" W=\"120\" H=\"30\" GlassRoughness=\"0.1\"/>\n    </Displays>\n"
        );
    }

    #[test]
    fn a_crlf_file_gets_a_crlf_block() {
        let doc = String::from_utf8_lossy(NO_DISPLAYS).replace('\n', "\r\n");
        let path = tmp_file("crlf.directb2s", doc.as_bytes());
        let plan = plan_at(&path, target(), None, 0.1).unwrap();
        assert!(plan.payload.contains("\r\n"));
        assert!(!plan.payload.replace("\r\n", " ").contains('\n'));
        apply(&plan, &fs::read(&path).unwrap(), false).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches("<DMD ").count(), 1);
        assert!(text.contains("    <Displays>\r\n        <DMD Target=\"Backglass\" X=\"10\""));
    }

    #[test]
    fn an_indented_root_end_tag_keeps_the_indentation() {
        let path = tmp_file(
            "indented.directb2s",
            b"<DirectB2SData>\n  <Images>\n  </Images>\n  </DirectB2SData>\n",
        );
        let plan = plan_at(&path, target(), None, 0.1).unwrap();
        apply(&plan, &fs::read(&path).unwrap(), false).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "<DirectB2SData>\n  <Images>\n  </Images>\n      <Displays>\n          <DMD Target=\"Backglass\" X=\"10\" Y=\"20\" W=\"120\" H=\"30\" GlassRoughness=\"0.1\"/>\n      </Displays>\n  </DirectB2SData>\n"
        );
    }

    #[test]
    fn crlf_inside_the_base64_payload_does_not_flip_the_tail() {
        // real files carry "\r\n" inside the base64 `P` attribute, while the
        // structural tail around `</Images>` is LF-only: the choice has to be
        // made from the tail, not from the file
        let doc = "<DirectB2SData>\n    <Images>\n        <BackglassImage P=\"aaa\r\nbbb\" P2=\"c\"/>\n    </Images>\n</DirectB2SData>\n";
        let path = tmp_file("payload.directb2s", doc.as_bytes());
        let plan = plan_at(&path, target(), None, 0.1).unwrap();
        assert!(!plan.payload.contains("\r\n"));
    }

    #[test]
    fn glass_roughness_keeps_its_decimals() {
        assert_eq!(fmt_glass(0.1), "0.1");
        assert_eq!(fmt_glass(0.25), "0.25");
        assert_eq!(fmt_glass(1.0), "1.0");
        assert!(tag_text(Rect::new(1.0, 2.0, 3.0, 4.0), 0.1).starts_with(
            "<DMD Target=\"Backglass\" X=\"1\" Y=\"2\" W=\"3\" H=\"4\" GlassRoughness=\"0.1\"/>"
        ));
    }

    #[test]
    fn a_file_without_the_root_end_tag_is_an_error() {
        let path = tmp_file("broken.directb2s", b"<DirectB2SData></Images>");
        assert!(plan_at(&path, target(), None, 0.1).is_err());
    }
}
