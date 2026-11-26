//! Writes the `<DMD>` elements into a B2S XML file, in place, keeping the
//! rest of the file byte-identical.
//!
//! These files are 25-53 MB because of the base64 images, so the edit is
//! targeted: locate the element in the tail of the file, splice the new text
//! in, copy everything else through unchanged. `<Displays>` is the last child
//! of the root element in the files B2S-Pro writes, so a missing block is
//! inserted directly before the root end tag.
//!
//! A file carries one `<DMD>` per display it overlays: `Target="Backglass"`
//! for the grill cut-out, `Target="ScoreView"` for the FullDMD area between
//! the speakers. Which of the two exist is the file's business (a grill
//! strip, a `DMDImage`); this module plans one action per requested target.
//!
//! The structural tail of these files is small, so the line ending used there
//! is detected and the inserted text follows it; the sample files use LF.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::b2s::Rect;
use crate::b2s::scan::{MatchKind, Record, stamp_targets};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Action {
    /// the file already holds this rectangle for this target
    Unchanged,
    /// rewrite the attributes of the existing `<DMD>` element of this target
    Replace,
    /// add the element, and the `<Displays>` block around it when missing
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
    /// which `<DMD Target="...">` this plan writes
    pub target: String,
    pub action: Action,
    /// byte span that `payload` replaces; an empty span is a pure insertion
    pub span: (usize, usize),
    pub payload: String,
    /// the rectangle currently in the file for this target
    pub from: Option<Rect>,
    pub to: Rect,
}

/// The `<DMD>` element text B2S-Pro writes for this target and rectangle.
pub fn tag_text(target: &str, rect: Rect, glass_roughness: f64) -> String {
    format!(
        "<DMD Target=\"{target}\" X=\"{:.0}\" Y=\"{:.0}\" W=\"{:.0}\" H=\"{:.0}\" GlassRoughness=\"{}\"/>",
        rect.x,
        rect.y,
        rect.w,
        rect.h,
        fmt_glass(glass_roughness)
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

/// One plan per requested target: replace the element of that target when the
/// file has one, insert it when it does not. A plan that fails (no root end
/// tag) fails the whole file, as before.
pub fn plan_all(
    path: &Path,
    bytes: &[u8],
    targets: &[(&str, Rect)],
    existing: &[(&str, Rect)],
    glass_roughness: f64,
) -> Result<Vec<Plan>, String> {
    let root_end = rfind(bytes, b"</DirectB2SData>").ok_or("no </DirectB2SData> end tag")?;
    // the image payloads sit above the structural tail, so only search below
    // the end of <Images>
    let tail = rfind(bytes, b"</Images>")
        .map(|p| p + b"</Images>".len())
        .unwrap_or(0)
        .min(root_end);
    let le = line_ending(bytes, tail);
    let base = format!("{}    ", indent_of(bytes, root_end));
    // the elements the file already carries, by target
    let mut plans = Vec::new();
    let mut missing: Vec<(&str, Rect)> = Vec::new();
    let spans = match find_from(bytes, b"<Displays", tail).filter(|p| *p < root_end) {
        Some(start) => {
            let limit = find_from(bytes, b"</Displays>", start + 9)
                .map(|p| p + b"</Displays>".len())
                .unwrap_or(root_end)
                .min(root_end);
            dmd_spans(bytes, start + 9, limit)
        }
        None => Vec::new(),
    };
    for (target, to) in targets {
        let found = spans
            .iter()
            .find(|(s, e)| span_target(bytes, *s, *e) == *target);
        // the file's own attribute text wins over the scan's memory of it:
        // compare the way the tag is written (`{:.0}`), not the raw floats,
        // so a fractional scan result that rounds to the stored numbers is
        // unchanged, not a rewrite of identical bytes
        let current = found
            .and_then(|(s, e)| span_rect(bytes, *s, *e))
            .or_else(|| existing.iter().find(|(t, _)| t == target).map(|(_, r)| *r));
        match found {
            Some((s, e)) => {
                let unchanged = current.map(|r| r.to_string()) == Some(to.to_string());
                plans.push(Plan {
                    path: path.to_path_buf(),
                    target: (*target).to_string(),
                    action: if unchanged {
                        Action::Unchanged
                    } else {
                        Action::Replace
                    },
                    span: (*s, *e),
                    payload: if unchanged {
                        String::new()
                    } else {
                        tag_text(target, *to, glass_roughness)
                    },
                    from: current,
                    to: *to,
                });
            }
            None => missing.push((*target, *to)),
        }
    }
    if missing.is_empty() {
        return Ok(plans);
    }
    // one insert plan carries all missing targets: they append at the same
    // point, and two plans at one point would undo each other
    let tags: Vec<String> = missing
        .iter()
        .map(|(t, r)| tag_text(t, *r, glass_roughness))
        .collect();
    let first_to = missing[0].1;
    if let Some((s, e)) = spans.last() {
        // append after the last `<DMD>` line, at its indentation
        let indent = indent_of(bytes, *s);
        // the insertion point is the end of the last tag's line, so each new
        // element is written as its own complete line
        let payload: String = missing
            .iter()
            .map(|(t, r)| format!("{indent}{}{le}", tag_text(t, *r, glass_roughness)))
            .collect();
        plans.push(Plan {
            path: path.to_path_buf(),
            target: missing
                .iter()
                .map(|(t, _)| *t)
                .collect::<Vec<_>>()
                .join("+"),
            action: Action::Insert,
            span: (line_end(bytes, *e), line_end(bytes, *e)),
            payload,
            from: None,
            to: first_to,
        });
        return Ok(plans);
    }
    if let Some(start) = find_from(bytes, b"<Displays", tail).filter(|p| *p < root_end) {
        // a <Displays> element with no <DMD>: replace a self-closing one, or
        // add the tag lines inside an open one
        if let Some(gt) = find_from(bytes, b">", start + 9).filter(|p| *p < root_end) {
            let indent = indent_of(bytes, start);
            let inner = format!("{indent}    ");
            let payload = if bytes[start + 9..=gt].contains(&b'/') {
                plans.push(Plan {
                    path: path.to_path_buf(),
                    target: missing
                        .iter()
                        .map(|(t, _)| *t)
                        .collect::<Vec<_>>()
                        .join("+"),
                    action: Action::Insert,
                    span: (start, gt + 1),
                    payload: format!(
                        "{indent}<Displays>{le}{inner}{}{le}{indent}</Displays>",
                        tags.join(le)
                    ),
                    from: None,
                    to: first_to,
                });
                return Ok(plans);
            } else {
                format!("{}{inner}{}", le, tags.join(&format!("{le}{inner}")))
            };
            plans.push(Plan {
                path: path.to_path_buf(),
                target: missing
                    .iter()
                    .map(|(t, _)| *t)
                    .collect::<Vec<_>>()
                    .join("+"),
                action: Action::Insert,
                span: (gt + 1, gt + 1),
                payload,
                from: None,
                to: first_to,
            });
            return Ok(plans);
        }
    }
    // no <Displays> at all: the whole block, the shape B2S-Pro writes
    plans.push(Plan {
        path: path.to_path_buf(),
        target: missing
            .iter()
            .map(|(t, _)| *t)
            .collect::<Vec<_>>()
            .join("+"),
        action: Action::Insert,
        span: (line_start(bytes, root_end), line_start(bytes, root_end)),
        payload: format!(
            "{base}<Displays>{le}{base}    {}{le}{base}</Displays>{le}",
            tags.join(&format!("{le}{base}    "))
        ),
        from: None,
        to: first_to,
    });
    Ok(plans)
}

/// Applies every plan and returns the size of the new file. The spans are
/// disjoint by construction; the rightmost goes first so the earlier offsets
/// stay valid. A `.bak` copy of the original is kept when `backup` is true.
pub fn apply_all(plans: &[Plan], bytes: &[u8], backup: bool) -> Result<usize, String> {
    let real: Vec<&Plan> = plans
        .iter()
        .filter(|p| p.action != Action::Unchanged)
        .collect();
    if real.is_empty() {
        return Ok(bytes.len());
    }
    let path = real[0].path.clone();
    if backup {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let bak = path.with_file_name(format!("{name}.bak"));
        // an existing `.bak` is the oldest state this tool has seen: keep it,
        // so a second run with other options cannot overwrite the original
        if !bak.exists() {
            fs::copy(&path, &bak).map_err(|e| format!("backup {}: {e}", bak.display()))?;
        }
    }
    // the spans are disjoint, so a single forward copy does it: everything
    // between two plans goes through byte-identical
    let mut plans = real.clone();
    plans.sort_by_key(|p| p.span.0);
    let mut out =
        Vec::with_capacity(bytes.len() + real.iter().map(|p| p.payload.len()).sum::<usize>());
    let mut cursor = 0;
    for plan in plans {
        out.extend_from_slice(&bytes[cursor..plan.span.0]);
        out.extend_from_slice(plan.payload.as_bytes());
        cursor = plan.span.1.max(cursor);
    }
    out.extend_from_slice(&bytes[cursor..]);
    // write beside the original, then swap, so an interrupt cannot leave a
    // half-written backglass
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, &out).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    fs::rename(&tmp, &path).map_err(|e| format!("replace {}: {e}", path.display()))?;
    Ok(out.len())
}

/// Single-plan apply, kept for callers with exactly one target.
pub fn apply(plan: &Plan, bytes: &[u8], backup: bool) -> Result<usize, String> {
    apply_all(std::slice::from_ref(plan), bytes, backup)
}

#[derive(Debug, Default)]
pub struct Stats {
    pub seen: usize,
    pub unchanged: usize,
    pub replaced: usize,
    pub inserted: usize,
    pub written: usize,
    pub skipped: usize,
    /// files left alone because they already carry a `<DMD>` tag
    pub existing: usize,
    pub duplicate: usize,
    pub failed: usize,
}

/// Plans (and with `write`, applies) the stamp for every record that has a
/// rectangle. Each record stamps the targets its file supports: the
/// `Backglass` element where a grill strip exists, the `ScoreView` element
/// where the file carries a `DMDImage`. A file that already carries a `<DMD>`
/// tag is the marker of a finished backglass - authored by a B2S editor or
/// stamped by an earlier run - and is left alone unless `force` asks to
/// overwrite it. A plan that fails does not stop the run.
pub fn stamp(
    records: &[Record],
    glass_roughness: f64,
    write: bool,
    backup: bool,
    force: bool,
) -> Stats {
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
        let (Some(path), Some(_)) = (rec.b2s_path.as_deref(), rec.chosen) else {
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
        let has_tag = rec.existing_dmd.is_some() || !rec.existing_tags.is_empty();
        if has_tag && !force {
            stats.existing += 1;
            let _ =
                crate::println!("  skip      {name}  existing <DMD> tag, use --force to overwrite");
            continue;
        }
        let targets = stamp_targets(rec);
        if targets.is_empty() {
            // a rect was chosen but the file supports no target: no grill
            // strip and no DMDImage, handled by the scan notes
            stats.skipped += 1;
            continue;
        }
        let owned: Vec<(&str, Rect)> = targets.iter().map(|(t, r)| (t.as_str(), *r)).collect();
        let existing: Vec<(&str, Rect)> = rec
            .existing_tags
            .iter()
            .map(|(t, r)| (t.as_str(), *r))
            .collect();
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) => {
                stats.failed += 1;
                let _ = crate::eprintln!("  error   {name}: read: {e}");
                continue;
            }
        };
        match plan_all(path, &bytes, &owned, &existing, glass_roughness) {
            Ok(plans) => {
                for plan in &plans {
                    match plan.action {
                        Action::Unchanged => stats.unchanged += 1,
                        Action::Replace => stats.replaced += 1,
                        Action::Insert => stats.inserted += 1,
                    }
                    let _ = crate::println!(
                        "  {:<9} {}  [{:<9}] {} -> {}",
                        plan.action,
                        name,
                        plan.target,
                        plan.from
                            .map(|r| r.to_string())
                            .unwrap_or_else(|| "-".into()),
                        plan.to
                    );
                }
                if write {
                    match apply_all(&plans, &bytes, backup) {
                        Ok(_) => {
                            if plans.iter().any(|p| p.action != Action::Unchanged) {
                                stats.written += 1;
                            }
                        }
                        Err(e) => {
                            stats.failed += 1;
                            let _ = crate::eprintln!("  error   {name}: {e}");
                        }
                    }
                }
            }
            Err(e) => {
                stats.failed += 1;
                let _ = crate::eprintln!("  error   {name}: {e}");
            }
        }
    }
    stats
}

pub fn print_stats(stats: &Stats, write: bool) {
    let verb = if write { "written" } else { "to write" };
    let _ = crate::println!("\n{}", if write { "stamped" } else { "dry run" });
    let _ = crate::println!("  with a rect       {}", stats.seen);
    let _ = crate::println!("  unchanged         {}", stats.unchanged);
    let _ = crate::println!("  replace tag       {}", stats.replaced);
    let _ = crate::println!("  insert tag/block  {}", stats.inserted);
    let _ = crate::println!("  {verb:<18}{}", stats.written);
    if stats.existing > 0 {
        let verb = if write { "left" } else { "skip" };
        let _ = crate::println!("  existing tag {verb:5}{}", stats.existing);
    }
    if stats.skipped > 0 {
        let _ = crate::println!("  no rect / no b2s  {}", stats.skipped);
    }
    if stats.duplicate > 0 {
        let _ = crate::println!("  same b2s file     {}", stats.duplicate);
    }
    if stats.failed > 0 {
        let _ = crate::println!("  failed            {}", stats.failed);
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

/// Every `<DMD>` element inside `[from, to)`.
fn dmd_spans(bytes: &[u8], from: usize, to: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut cursor = from;
    while let Some(span @ (_, e)) = dmd_tag_span(bytes, cursor, to) {
        out.push(span);
        cursor = e;
    }
    out
}

/// The `Target` attribute of the tag in `[from, to)`; files without one are
/// the old single-target files, whose element is the backglass.
fn span_target(bytes: &[u8], from: usize, to: usize) -> String {
    let text = &bytes[from..to];
    let key = b"Target=\"";
    match find_from(text, key, 0) {
        Some(p) => {
            let start = from + p + key.len();
            let end = bytes[start..to]
                .iter()
                .position(|&c| c == b'"')
                .map(|p| start + p)
                .unwrap_or(start);
            String::from_utf8_lossy(&bytes[start..end]).into_owned()
        }
        None => "Backglass".to_string(),
    }
}

/// The rectangle stored in the tag in `[from, to)`.
fn span_rect(bytes: &[u8], from: usize, to: usize) -> Option<Rect> {
    let text = &bytes[from..to];
    let text = std::str::from_utf8(text).ok()?;
    let number = |key: &str| {
        let at = text.find(key)? + key.len();
        let end = text[at..].find('"')? + at;
        text[at..end].parse::<f64>().ok()
    };
    Some(Rect::new(
        number("X=\"")?,
        number("Y=\"")?,
        number("W=\"")?,
        number("H=\"")?,
    ))
}

/// The first byte of the line that holds `at`.
fn line_start(bytes: &[u8], at: usize) -> usize {
    match bytes[..at].iter().rposition(|&c| c == b'\n') {
        Some(p) => p + 1,
        None => at,
    }
}

/// The byte after the end of the line that holds `at` (the newline included).
fn line_end(bytes: &[u8], at: usize) -> usize {
    match bytes[at..].iter().position(|&c| c == b'\n') {
        Some(p) => at + p + 1,
        None => bytes.len(),
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

    fn bg() -> (&'static str, Rect) {
        ("Backglass", target())
    }

    /// reads the file, then plans, as the stamp pass does
    fn plan_at(
        path: &Path,
        targets: &[(&str, Rect)],
        existing: &[(&str, Rect)],
        glass: f64,
    ) -> Result<Vec<Plan>, String> {
        let bytes = fs::read(path).map_err(|e| e.to_string())?;
        plan_all(path, &bytes, targets, existing, glass)
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
        let plans = plan_at(&path, &[bg()], &[("Backglass", current)], 0.1).unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].action, Action::Replace);
        apply_all(&plans, &fs::read(&path).unwrap(), true).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(
            text,
            "<DirectB2SData Version=\"1.2\">\n    <Images>\n    </Images>\n    <Displays>\n        <DMD Target=\"Backglass\" X=\"10\" Y=\"20\" W=\"120\" H=\"30\" GlassRoughness=\"0.1\"/>\n    </Displays>\n</DirectB2SData>\n"
        );
        assert_eq!(text.matches("<DMD ").count(), 1);
        assert!(
            tmp_file("has.directb2s", WITH_TAG)
                .with_file_name("has.directb2s.bak")
                .exists()
        );
    }

    #[test]
    fn appends_a_displays_block_when_missing() {
        let path = tmp_file("missing.directb2s", NO_DISPLAYS);
        let plans = plan_at(&path, &[bg()], &[], 0.1).unwrap();
        assert_eq!(plans[0].action, Action::Insert);
        apply_all(&plans, &fs::read(&path).unwrap(), false).unwrap();
        let want = String::from_utf8_lossy(NO_DISPLAYS).replace(
            "</DirectB2SData>",
            &(plans[0].payload.clone() + "</DirectB2SData>"),
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
        let plans = plan_at(&path, &[bg()], &[], 0.1).unwrap();
        assert_eq!(plans[0].action, Action::Insert);
        apply_all(&plans, &fs::read(&path).unwrap(), false).unwrap();
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
        let plans = plan_at(&path, &[bg()], &[], 0.1).unwrap();
        apply_all(&plans, &fs::read(&path).unwrap(), false).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches("<Displays").count(), 1);
        assert_eq!(text.matches("<DMD ").count(), 1);
        assert!(text.ends_with("    </Displays>\n</DirectB2SData>\n"));
    }

    #[test]
    fn an_already_correct_tag_is_left_alone() {
        let path = tmp_file("same.directb2s", WITH_TAG);
        let rect = Rect::new(484.0, 1240.0, 1034.0, 218.0);
        let plans = plan_at(&path, &[("Backglass", rect)], &[("Backglass", rect)], 0.1).unwrap();
        assert_eq!(plans[0].action, Action::Unchanged);
        apply_all(&plans, &fs::read(&path).unwrap(), false).unwrap();
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
        let plans = plan_at(&path, &[("Backglass", to)], &[], 0.1).unwrap();
        assert_eq!(plans[0].action, Action::Unchanged);
        assert!(plans[0].payload.is_empty());
    }

    #[test]
    fn the_second_run_finds_nothing_to_do() {
        let path = tmp_file("idempotent.directb2s", NO_DISPLAYS);
        let to = Rect::new(10.4, 20.6, 120.6, 30.6);
        let first = plan_at(&path, &[("Backglass", to)], &[], 0.1).unwrap();
        assert_eq!(first[0].action, Action::Insert);
        apply_all(&first, &fs::read(&path).unwrap(), false).unwrap();
        let stamped = fs::read(&path).unwrap();
        // the scan sees no tag before the first run, and the rounded values
        // after it: the second plan must not rewrite the file
        let second = plan_all(
            &path,
            &stamped,
            &[("Backglass", to)],
            &[("Backglass", Rect::new(10.0, 21.0, 121.0, 31.0))],
            0.1,
        )
        .unwrap();
        assert_eq!(second[0].action, Action::Unchanged);
        assert_eq!(fs::read(&path).unwrap(), stamped);
    }

    #[test]
    fn the_scoreview_target_joins_an_existing_backglass_tag() {
        // the user's shape: both targets, the same rectangle, the new element
        // lands below the existing one at the same indentation
        let path = tmp_file("scoreview.directb2s", WITH_TAG);
        let current = Rect::new(484.0, 1240.0, 1034.0, 218.0);
        let plans = plan_at(
            &path,
            &[
                ("Backglass", current),
                ("ScoreView", Rect::new(484.0, 1240.0, 1034.0, 218.0)),
            ],
            &[("Backglass", current)],
            0.1,
        )
        .unwrap();
        assert_eq!(plans[0].action, Action::Unchanged);
        assert_eq!(plans[1].action, Action::Insert);
        apply_all(&plans, &fs::read(&path).unwrap(), false).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(
            text,
            "<DirectB2SData Version=\"1.2\">\n    <Images>\n    </Images>\n    <Displays>\n        <DMD Target=\"Backglass\" X=\"484\" Y=\"1240\" W=\"1034\" H=\"218\" GlassRoughness=\"0.1\"/>\n        <DMD Target=\"ScoreView\" X=\"484\" Y=\"1240\" W=\"1034\" H=\"218\" GlassRoughness=\"0.1\"/>\n    </Displays>\n</DirectB2SData>\n"
        );
    }

    #[test]
    fn both_targets_land_in_one_new_block() {
        let path = tmp_file("both.directb2s", NO_DISPLAYS);
        let plans = plan_at(
            &path,
            &[bg(), ("ScoreView", Rect::new(760.0, 2320.0, 1475.0, 376.0))],
            &[],
            0.1,
        )
        .unwrap();
        assert_eq!(plans.len(), 1);
        apply_all(&plans, &fs::read(&path).unwrap(), false).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches("<Displays").count(), 1);
        assert_eq!(text.matches("<DMD ").count(), 2);
        assert!(text.contains("<DMD Target=\"Backglass\" X=\"10\" Y=\"20\" W=\"120\" H=\"30\""));
        assert!(
            text.contains("<DMD Target=\"ScoreView\" X=\"760\" Y=\"2320\" W=\"1475\" H=\"376\"")
        );
    }

    #[test]
    fn a_stamped_scoreview_tag_is_replaced_not_duplicated() {
        let doc = String::from_utf8_lossy(WITH_TAG).replace(
            "/>\n    </Displays>",
            "/>\n        <DMD Target=\"ScoreView\" X=\"1\" Y=\"1\" W=\"1\" H=\"1\" GlassRoughness=\"0.1\"/>\n    </Displays>",
        );
        let path = tmp_file("hassv.directb2s", doc.as_bytes());
        let plans = plan_at(
            &path,
            &[
                ("Backglass", Rect::new(484.0, 1240.0, 1034.0, 218.0)),
                ("ScoreView", target()),
            ],
            &[],
            0.1,
        )
        .unwrap();
        assert_eq!(plans[0].action, Action::Unchanged);
        assert_eq!(plans[1].action, Action::Replace);
        apply_all(&plans, &fs::read(&path).unwrap(), false).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches("<DMD ").count(), 2);
        assert!(text.contains("<DMD Target=\"ScoreView\" X=\"10\" Y=\"20\" W=\"120\" H=\"30\""));
    }

    #[test]
    fn an_existing_tag_is_left_alone_unless_forced() {
        let path = tmp_file("marker.directb2s", WITH_TAG);
        let bytes = fs::read(&path).unwrap();
        let rec = Record {
            vpx: "a.vpx".into(),
            b2s_path: Some(path.clone()),
            match_kind: MatchKind::Exact,
            existing_dmd: Some(Rect::new(484.0, 1240.0, 1034.0, 218.0)),
            chosen: Some(target()),
            grill_height: Some(240.0),
            ..Default::default()
        };
        let recs = [rec];
        // the tag is the marker of a finished backglass: a rescan skips it
        let stats = stamp(&recs, 0.1, true, false, false);
        assert_eq!(stats.existing, 1);
        assert_eq!(stats.replaced, 0);
        assert_eq!(stats.written, 0);
        assert_eq!(fs::read(&path).unwrap(), bytes);
        // --force overwrites it
        let stats = stamp(&recs, 0.1, true, false, true);
        assert_eq!(stats.replaced, 1);
        assert_eq!(stats.written, 1);
        assert_ne!(fs::read(&path).unwrap(), bytes);
    }

    #[test]
    fn an_existing_backup_is_not_overwritten() {
        let path = tmp_file("keepbak.directb2s", WITH_TAG);
        let bak = path.with_file_name("keepbak.directb2s.bak");
        let _ = fs::remove_file(&bak);
        let current = Rect::new(484.0, 1240.0, 1034.0, 218.0);
        let first = plan_at(
            &path,
            &[("Backglass", target())],
            &[("Backglass", current)],
            0.1,
        )
        .unwrap();
        apply_all(&first, &fs::read(&path).unwrap(), true).unwrap();
        let original = fs::read(&bak).unwrap();
        let second = plan_at(
            &path,
            &[("Backglass", Rect::new(1.0, 2.0, 3.0, 4.0))],
            &[("Backglass", current)],
            0.1,
        )
        .unwrap();
        apply_all(&second, &fs::read(&path).unwrap(), true).unwrap();
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
            grill_height: Some(240.0),
            ..Default::default()
        };
        let second = Record {
            vpx: "b.vpx".into(),
            b2s_path: Some(path.clone()),
            match_kind: MatchKind::Exact,
            chosen: Some(rect),
            grill_height: Some(240.0),
            ..Default::default()
        };
        let stats = stamp(&[first, second], 0.1, true, false, false);
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
        let plans = plan_at(&path, &[bg()], &[], 0.1).unwrap();
        // B2S Pro writes the block at one indent below `</Images>`, the element
        // at two, with the attributes in this order
        assert_eq!(
            plans[0].payload,
            "    <Displays>\n        <DMD Target=\"Backglass\" X=\"10\" Y=\"20\" W=\"120\" H=\"30\" GlassRoughness=\"0.1\"/>\n    </Displays>\n"
        );
    }

    #[test]
    fn a_crlf_file_gets_a_crlf_block() {
        let doc = String::from_utf8_lossy(NO_DISPLAYS).replace('\n', "\r\n");
        let path = tmp_file("crlf.directb2s", doc.as_bytes());
        let plans = plan_at(&path, &[bg()], &[], 0.1).unwrap();
        assert!(plans[0].payload.contains("\r\n"));
        assert!(!plans[0].payload.replace("\r\n", " ").contains('\n'));
        apply_all(&plans, &fs::read(&path).unwrap(), false).unwrap();
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
        let plans = plan_at(&path, &[bg()], &[], 0.1).unwrap();
        apply_all(&plans, &fs::read(&path).unwrap(), false).unwrap();
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
        let plans = plan_at(&path, &[bg()], &[], 0.1).unwrap();
        assert!(!plans[0].payload.contains("\r\n"));
    }

    #[test]
    fn glass_roughness_keeps_its_decimals() {
        assert_eq!(fmt_glass(0.1), "0.1");
        assert_eq!(fmt_glass(0.25), "0.25");
        assert_eq!(fmt_glass(1.0), "1.0");
        assert!(
            tag_text("Backglass", Rect::new(1.0, 2.0, 3.0, 4.0), 0.1).starts_with(
                "<DMD Target=\"Backglass\" X=\"1\" Y=\"2\" W=\"3\" H=\"4\" GlassRoughness=\"0.1\"/>"
            )
        );
    }

    #[test]
    fn a_file_without_the_root_end_tag_is_an_error() {
        let path = tmp_file("broken.directb2s", b"<DirectB2SData></Images>");
        assert!(plan_at(&path, &[bg()], &[], 0.1).is_err());
    }
}
