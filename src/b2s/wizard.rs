//! The calibration wizard: `vpxtool b2s wizard <TABLES>`.
//!
//! The ini method maps the desktop coordinates of DmdDevice.ini into the
//! backglass image, and that needs the backglass monitor rectangle: where it
//! sits on the desktop right of the playfield. vpxtool's default
//! 2560,0,2560,2880 is a guess; on a 4K playfield it slides every stamp by
//! over a thousand pixels.
//!
//! The sizes come from the files: ScreenRes.txt names the playfield window
//! and, in the V6 format, the backglass window - in the size B2S operates
//! at, which is the space of the ini coordinates. The position is in no
//! file: the backglass window's spot on the desktop is Windows display
//! placement, free to be anything, negative coordinates included. So the
//! wizard asks for it, with the default arrangement (right of the playfield,
//! flush with the desktop top) as the default answer - and lets the tables
//! themselves measure the truth.
//! The wizard reads those, then *verifies* the geometry against the user's
//! own tables: for every table with both a detected DMD window (hole
//! detection) and a DmdDevice.ini section, the ini rect is mapped into the
//! backglass image and the distance to the detected window is the per-table
//! residual. A small median residual means the geometry is right. The wizard
//! shows the numbers, takes the user's corrections, and writes
//! `b2s_backglass_monitor` into the vpxtool config.

use std::io::IsTerminal;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use super::Rect;
use super::dmdini::DmdDeviceIni;
use super::scan::{Record, ScanArgs, Target};

pub struct WizardArgs {
    pub target: Target,
    pub ini: Option<PathBuf>,
    pub jobs: usize,
    /// accept the findings without asking (non-interactive use)
    pub yes: bool,
}

/// The monitor size the tables themselves imply: a table's ini DMD window is
/// ini_w monitor pixels wide, and its painted window is hole_w pixels in an
/// image of image_w - so the monitor is image_w * ini_w / hole_w pixels in
/// the ini's space. The painted window carries a bezel around the DMD, so
/// this overestimates a little; it is a check of the answers, not a
/// replacement for them. (position, 0)
fn estimated_monitor_size(records: &[Record]) -> Option<(f64, f64)> {
    let mut ws: Vec<f64> = Vec::new();
    let mut hs: Vec<f64> = Vec::new();
    for rec in records {
        let (Some(h), Some(i)) = (rec.hole_backglass, rec.ini_rect) else {
            continue;
        };
        let Some((w, h_img)) = rec.backglass_size else {
            continue;
        };
        if h.w > 1.0 && h.h > 1.0 && i.w > 1.0 && i.h > 1.0 {
            ws.push(w as f64 * i.w / h.w);
            hs.push(h_img as f64 * i.h / h.h);
        }
    }
    if ws.len() < 8 {
        return None;
    }
    ws.sort_by(|a, b| a.partial_cmp(b).unwrap());
    hs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Some((ws[ws.len() / 2], hs[hs.len() / 2]))
}

/// The screen geometry as far as it is knowable: the playfield width, the
/// x at which the backglass window starts in the default arrangement. The
/// y is not derivable from any file - Windows display placement is free
/// (negative y included) - so the starting point puts y0 at 0 and the
/// tables measure the truth.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    pub x: f64,
    pub y: f64,
}

impl Geometry {
    pub fn monitor(&self, backglass_w: f64, backglass_h: f64) -> Rect {
        Rect::new(self.x, self.y, backglass_w, backglass_h)
    }
}

/// The numbers ScreenRes.txt can be trusted for: the playfield window size,
/// and - in the V6 format - the backglass window size. That size is the one
/// B2S operates at, the size in the space of the ini coordinates: a 1707
/// wide panel at 150% scaling shows up as 2560 there. Everything past the
/// first four values (screen index, DMD area, decorations) is relative to
/// the B2S window, not to the desktop, and is not used.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScreenResInfo {
    pub playfield_w: f64,
    pub playfield_h: f64,
    pub backglass: Option<(f64, f64)>,
}

/// Reads ScreenRes.txt by section: the numbers under a `# ... Playfield ...
/// resolution ...` comment are the playfield window size, the numbers under
/// `# Backglass width/height` are the backglass window size. The old format
/// has no backglass section (its sizes are `100% 100%` percentages, not
/// pixels), so the backglass is None there. One value per line, values also
/// share lines, comments start with `#`, `%` values are not plain numbers.
pub fn read_screenres(path: &Path) -> io::Result<ScreenResInfo> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| io::Error::other(format!("{}: {e}", path.display())))?;
    let mut section = String::new();
    let mut pf: Vec<f64> = Vec::new();
    let mut bg: Vec<f64> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            section = trimmed.to_lowercase();
            continue;
        }
        for token in trimmed.split_whitespace() {
            let Ok(value) = token.parse::<f64>() else {
                continue;
            };
            if section.contains("playfield") && section.contains("resolution") && pf.len() < 2 {
                pf.push(value);
            } else if section.contains("backglass width") && bg.len() < 2 {
                bg.push(value);
            }
        }
    }
    if pf.len() < 2 {
        // the old format's header is "#Playfield resolution" - covered above;
        // as a last resort, the first two numbers of the file are the
        // playfield size in every format known so far
        pf = text
            .lines()
            .filter(|l| !l.trim().starts_with('#'))
            .flat_map(|l| l.split_whitespace())
            .filter_map(|t| t.parse::<f64>().ok())
            .take(2)
            .collect();
    }
    if pf.len() < 2 {
        return Err(io::Error::other(
            "ScreenRes.txt: not enough numbers to find the playfield size",
        ));
    }
    Ok(ScreenResInfo {
        playfield_w: pf[0],
        playfield_h: pf[1],
        backglass: if bg.len() == 2 {
            Some((bg[0], bg[1]))
        } else {
            None
        },
    })
}

fn prompt_line(question: &str, default: Option<&str>, interactive: bool) -> io::Result<String> {
    if !interactive {
        return Ok(default.unwrap_or_default().to_string());
    }
    match default {
        Some(d) => crate::print!("{question} [{d}]: ")?,
        None => crate::print!("{question}: ")?,
    }
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let answer = input.trim();
    if answer.is_empty() {
        return Ok(default.unwrap_or_default().to_string());
    }
    Ok(answer.to_string())
}

/// Asks for a number, `default` shown in brackets and taken on an empty
/// answer.
fn prompt_number(question: &str, default: f64, interactive: bool) -> io::Result<f64> {
    let answer = prompt_line(question, Some(&format!("{default:.0}")), interactive)?;
    answer
        .parse::<f64>()
        .map_err(|e| io::Error::other(format!("{question}: {answer:?} is not a number: {e}")))
}

/// The Visual Pinball executable, found by walking up from DmdDevice.ini.
/// The ini lives in the VPinMAME folder of the install; the exe sits either
/// in the install root, in the Baller installer's `Visual Pinball` (and
/// `VisualPinball`) subfolder of it, or in any other direct subfolder of the
/// root. Three levels up from the ini is the last folder examined, and at
/// the root only its direct children - so a stray `D:\` is never walked.
pub fn detect_vpx_executable(ini_path: &Path) -> Option<PathBuf> {
    const NAMES: [&str; 5] = [
        "VPinballX_64.exe",
        "VPinballX64.exe",
        "VPinballX.exe",
        "VPinballX_GL",
        "vpinballx_gl",
    ];
    let find = |dir: &Path| -> Option<PathBuf> {
        NAMES
            .iter()
            .map(|name| dir.join(name))
            .find(|candidate| candidate.is_file())
    };
    let mut dir = ini_path.parent();
    let mut depth = 0;
    while let Some(d) = dir.filter(|_| depth < 4) {
        if let Some(exe) = find(d) {
            return Some(exe);
        }
        // the Baller layout: <root>\Visual Pinball\VPinballX_64.exe - and
        // any other one-level layout, so scan the direct subfolders
        if depth >= 1
            && let Ok(entries) = std::fs::read_dir(d)
        {
            for entry in entries.flatten() {
                if let Some(exe) = find(&entry.path()) {
                    return Some(exe);
                }
            }
        }
        dir = d.parent();
        depth += 1;
    }
    None
}

fn confirm(question: &str, interactive: bool) -> io::Result<bool> {
    let answer = prompt_line(&format!("{question} (y/n)"), Some("y"), interactive)?;
    Ok(answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes"))
}

/// `x,y` with optional whitespace; None when not two numbers.
fn parse_xy(text: &str) -> Option<(f64, f64)> {
    let (a, b) = text.trim().split_once(',')?;
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
}

/// Values that do not hang together, in words. All of it is computable
/// from the two files the wizard feeds on: every ini window must fit
/// inside the backglass monitor (the ini coords are desktop pixels of that
/// monitor's space), sizes must be in a plausible range, an ini with two
/// sections is probably the wrong file, and answers that disagree with
/// ScreenRes.txt mean one of the two is stale.
fn sanity_problems(
    x: f64,
    y: f64,
    bg_w: f64,
    bg_h: f64,
    pf_w: f64,
    ini: &DmdDeviceIni,
    screenres: Option<ScreenResInfo>,
) -> Vec<String> {
    let mut out = Vec::new();
    let sane = |n: f64| (32.0..=16384.0).contains(&n);
    for (what, n) in [
        ("playfield width", pf_w),
        ("backglass width", bg_w),
        ("backglass height", bg_h),
    ] {
        if !sane(n) {
            out.push(format!("{what} {n:.0} px is outside the 32..16384 range"));
        }
    }
    if x.abs() > 32768.0 || y.abs() > 32768.0 {
        out.push(format!(
            "position {x:.0},{y:.0} is outside the +-32768 range"
        ));
    }
    if ini.section_count() < 5 {
        out.push(format!(
            "the ini has only {} sections - the right file?",
            ini.section_count()
        ));
    }
    // every ini window sits on the backglass monitor - in real files a few
    // are dragged off to other screens, so the check is the majority: with
    // the right monitor almost all windows fit, with a wrong one almost
    // none do (measured on a real cabinet: 5 % against 96 %)
    let rects = ini.window_rects();
    if rects.len() >= 8 {
        // a hair of tolerance: the windows sit inside the monitor with a few
        // pixels to spare at most edges, title bars do not reach exactly
        let (mx, my) = (0.02 * bg_w, 0.02 * bg_h);
        let outside = rects
            .iter()
            .filter(|r| {
                r.x < x - mx
                    || r.x + r.w > x + bg_w + mx
                    || r.y < y - my
                    || r.y + r.h > y + bg_h + my
            })
            .count();
        if outside * 5 > rects.len() {
            out.push(format!(
                "{outside} of {} ini DMD windows fall outside the backglass monitor {x:.0},{y:.0},{bg_w:.0}x{bg_h:.0} - check the position and the sizes",
                rects.len()
            ));
        }
    }
    if let Some(sr) = screenres
        && let Some((w, h)) = sr.backglass
        && ((bg_w - w).abs() > 0.2 * w || (bg_h - h).abs() > 0.2 * h)
    {
        // a clean ratio is not staleness, it is the display scaling: the
        // answer is the physical panel, ScreenRes.txt and the ini live in
        // the scaled space - say so in numbers, it is the one confusion
        // this wizard keeps running into
        let (rx, ry) = (w / bg_w, h / bg_h);
        if (rx - ry).abs() < 0.03 && (1.05..2.6).contains(&rx) {
            out.push(format!(
                "ScreenRes.txt says {w:.0}x{h:.0} and the answers {bg_w:.0}x{bg_h:.0} differ by a ratio of {:.0}%: that is the display scaling - the panel is {bg_w:.0}x{bg_h:.0} physical pixels, and the B2S server, vpinmame and DmdDevice.ini run in the scaled space, where it is {w:.0}x{h:.0}; answer the scaled size, and multiply the position by the same ratio",
                rx * 100.0
            ));
        } else {
            out.push(format!(
                "ScreenRes.txt says {w:.0}x{h:.0}, the answers say {bg_w:.0}x{bg_h:.0} - one of the two is stale"
            ));
        }
    }
    out
}

fn median(values: &[f64]) -> f64 {
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

/// The median signed distance (in desktop pixels) from the ini rect, mapped
/// with the candidate monitor, to the detected window. With a tolerance, the
/// tables further away than that are dropped: they are the ones whose
/// virtual DMD was dragged off the painted cut-out and must not pull the
/// median. Without one the median runs over all pairs - the first round on a
/// bad candidate, where the cluster is the whole set minus a few moved
/// windows, is still the median of the cluster.
fn median_shift(records: &[Record], monitor: &Rect, axis_x: bool, tol: Option<f64>) -> Option<f64> {
    let mut d: Vec<f64> = Vec::new();
    for rec in records {
        let (Some(h), Some(i)) = (rec.hole_backglass, rec.ini_rect) else {
            continue;
        };
        let Some((w, h_img)) = rec.backglass_size else {
            continue;
        };
        let (sx, sy) = (w as f64 / monitor.w, h_img as f64 / monitor.h);
        let (dx, dy) = (
            ((i.x - monitor.x) * sx + i.w * sx / 2.0) - (h.x + h.w / 2.0),
            ((i.y - monitor.y) * sy + i.h * sy / 2.0) - (h.y + h.h / 2.0),
        );
        if tol.is_some_and(|t| dx.abs() > t) {
            continue;
        }
        d.push(if axis_x { dx / sx } else { dy / sy });
    }
    (!d.is_empty()).then(|| median(&d))
}

/// The unsigned residuals of the ini mapping against the detected windows.
fn residuals(records: &[Record], monitor: &Rect) -> (Vec<f64>, Vec<f64>, usize) {
    let mut dxs: Vec<f64> = Vec::new();
    let mut dys: Vec<f64> = Vec::new();
    let mut pairs = 0;
    for rec in records {
        let (Some(h), Some(i)) = (rec.hole_backglass, rec.ini_rect) else {
            continue;
        };
        let Some((w, h_img)) = rec.backglass_size else {
            continue;
        };
        let (sx, sy) = (w as f64 / monitor.w, h_img as f64 / monitor.h);
        dxs.push(((i.x - monitor.x) * sx + i.w * sx / 2.0 - (h.x + h.w / 2.0)).abs());
        dys.push(((i.y - monitor.y) * sy + i.h * sy / 2.0 - (h.y + h.h / 2.0)).abs());
        pairs += 1;
    }
    (dxs, dys, pairs)
}

fn rect_string(r: &Rect) -> String {
    format!("{:.0},{:.0},{:.0},{:.0}", r.x, r.y, r.w, r.h)
}

pub fn run(args: &WizardArgs) -> io::Result<()> {
    let interactive = std::io::stdin().is_terminal() && !args.yes;

    // the previous run's answers are the defaults of this one
    let answers = crate::config::b2s_answers();

    // ---- step 1: DmdDevice.ini
    crate::println!("step 1: DmdDevice.ini")?;
    let found = args
        .ini
        .clone()
        .or_else(|| answers.ini.clone().filter(|p| p.is_file()))
        .unwrap_or_else(|| super::default_ini(&[]));
    crate::println!("  candidate: {}", found.display())?;
    let answer = prompt_line(
        "  path of DmdDevice.ini",
        Some(&found.to_string_lossy()),
        interactive,
    )?;
    let ini_path = PathBuf::from(answer);
    let ini = DmdDeviceIni::load(&ini_path).map_err(io::Error::other)?;
    crate::println!(
        "  loaded: {} ({} rom sections)",
        ini_path.display(),
        ini.section_count()
    )?;

    // ---- step 2: the screen setup, from ScreenRes.txt when it is there.
    // ScreenRes.txt is a hint, not the answer - a resolution change is one
    // click in the Windows settings and the file is only rewritten by the
    // B2S setup - so its numbers are the defaults of the questions, and the
    // position, which no file knows, is asked outright. The candidate is
    // verified against the tables in step 4, and their data corrects it.
    crate::println!(
        "step 2: the screen setup (the default arrangement puts the backglass window right of the playfield)"
    )?;
    crate::println!(
        "  the position of the backglass monitor on the desktop is Windows display placement, no file knows it - the tables measure it in step 4"
    )?;
    let found = match &args.target {
        Target::Directory(d) => Some(d.join("ScreenRes.txt")),
        _ => None,
    }
    .filter(|p| p.is_file())
    .or_else(|| answers.screenres.clone().filter(|p| p.is_file()));
    // the file sits with the B2S server, not necessarily with the tables -
    // so it is asked for, with what was found as the default
    match &found {
        Some(p) => crate::println!("  ScreenRes.txt found: {}", p.display())?,
        None => crate::println!("  no ScreenRes.txt in the table folder")?,
    }
    let default_path = found
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let answer = prompt_line(
        "  path to ScreenRes.txt (empty to go without)",
        Some(&default_path),
        interactive,
    )?;
    let screenres = if answer.trim().is_empty() {
        None
    } else {
        let path = PathBuf::from(answer.trim());
        if !path.is_file() {
            return Err(io::Error::other(format!(
                "no ScreenRes.txt at {}",
                path.display()
            )));
        }
        Some(path)
    };
    let screenres_info = match &screenres {
        Some(path) => {
            let info = read_screenres(path)?;
            crate::println!(
                "  ScreenRes.txt: playfield window {:.0}x{:.0}{}",
                info.playfield_w,
                info.playfield_h,
                match info.backglass {
                    Some((w, h)) =>
                        format!(", backglass window {w:.0}x{h:.0} (the size in the ini's space)"),
                    None => String::new(),
                }
            )?;
            Some(info)
        }
        None => None,
    };
    // the file can be stale - a resolution change is one click in the
    // Windows settings, ScreenRes.txt is only rewritten by the B2S setup -,
    // so its numbers are the defaults of the questions, not the answers
    // the sizes come from ScreenRes.txt, the position is the one number no
    // file has - so it is asked, with the default arrangement as its default
    let saved_monitor = answers
        .monitor
        .as_deref()
        .and_then(|t| t.parse::<Rect>().ok());
    // the one question that unlocks all the other numbers: every size and
    // position in the Windows display settings is physical pixels, while
    // the B2S server, vpinmame and DmdDevice.ini run in the scaled space.
    // The answers are taken in physical pixels and multiplied by the
    // scaling into the space the ini lives in.
    let mut scaling = prompt_number(
        "  scaling of the backglass monitor in percent, from the Windows display settings (150 for 150%)",
        answers.scaling.map(|v| v as f64).unwrap_or(100.0),
        interactive,
    )?;
    if !(25.0..=400.0).contains(&scaling) {
        return Err(io::Error::other(format!(
            "scaling {scaling:.0}% is outside the 25..400 range"
        )));
    }
    let mut scale = scaling / 100.0;
    // the playfield is taken as 100%: it is the primary monitor, and the
    // scaled desktop is anchored at its top left corner
    let mut playfield_w = prompt_number(
        "  playfield screen width (pixels): the default arrangement starts the backglass at this x",
        screenres_info
            .map(|i| i.playfield_w)
            .or_else(|| answers.playfield_w.map(|w| w as f64))
            .unwrap_or(2560.0),
        interactive,
    )?;
    // ScreenRes.txt states the scaled size, the saved monitor is scaled
    // too: both are divided back into the physical numbers of the settings
    let mut bg_w = prompt_number(
        "  backglass screen width in the display settings (physical pixels)",
        screenres_info
            .and_then(|i| i.backglass)
            .map(|(w, _)| w)
            .or_else(|| saved_monitor.map(|m| m.w))
            .unwrap_or(2560.0)
            / scale,
        interactive,
    )?;
    let mut bg_h = prompt_number(
        "  backglass screen height in the display settings (physical pixels)",
        screenres_info
            .and_then(|i| i.backglass)
            .map(|(_, h)| h)
            .or_else(|| saved_monitor.map(|m| m.h))
            .unwrap_or(2880.0)
            / scale,
        interactive,
    )?;
    let mut position = prompt_line(
        "  backglass window position as the display settings show it, x,y (physical pixels, negative allowed)",
        Some(
            &saved_monitor
                .map(|m| format!("{:.0},{:.0}", m.x / scale, m.y / scale))
                .unwrap_or_else(|| format!("{playfield_w:.0},0")),
        ),
        interactive,
    )?;
    let (mut px, mut py) = parse_xy(&position).ok_or_else(|| {
        io::Error::other(format!(
            "not a position: {position} - expected two numbers, x,y"
        ))
    })?;
    // everything that looks fishy gets asked about, with the current values
    // as the defaults of the second round; the checks run on the computed
    // ini-space rectangle
    loop {
        let problems = sanity_problems(
            px * scale,
            py * scale,
            bg_w * scale,
            bg_h * scale,
            playfield_w,
            &ini,
            screenres_info,
        );
        if problems.is_empty() {
            break;
        }
        for p in &problems {
            crate::println!("    check: {p}")?;
        }
        crate::println!(
            "  the numbers are multiplied by the scaling ({scaling:.0}%) into the ini's space; a wrong scaling explains a lot: the display settings show physical pixels, the ini lives in the scaled ones"
        )?;
        if confirm("  the values look off, keep them anyway", interactive)? {
            break;
        }
        let scaling_new = prompt_number(
            "  scaling of the backglass monitor in percent",
            scaling,
            interactive,
        )?;
        if (25.0..=400.0).contains(&scaling_new) {
            scaling = scaling_new;
            scale = scaling / 100.0;
        } else {
            crate::println!("  keeping {scaling:.0}%: {scaling_new:.0}% is outside 25..400")?;
        }
        playfield_w = prompt_number(
            "  playfield screen width (pixels)",
            playfield_w,
            interactive,
        )?;
        bg_w = prompt_number(
            "  backglass screen width (physical pixels)",
            bg_w,
            interactive,
        )?;
        bg_h = prompt_number(
            "  backglass screen height (physical pixels)",
            bg_h,
            interactive,
        )?;
        position = prompt_line(
            "  backglass window position, x,y (physical pixels)",
            Some(&format!("{px:.0},{py:.0}")),
            interactive,
        )?;
        let (nx, ny) = parse_xy(&position).ok_or_else(|| {
            io::Error::other(format!(
                "not a position: {position} - expected two numbers, x,y"
            ))
        })?;
        px = nx;
        py = ny;
    }
    // the answers in the ini's space: physical pixels times the scaling,
    // the space of the desktop the ini, B2S and vpinmame see
    let bg_w = bg_w * scale;
    let bg_h = bg_h * scale;
    let geometry = Geometry {
        x: px * scale,
        y: py * scale,
    };
    crate::println!(
        "  in the ini's space: {}",
        rect_string(&geometry.monitor(bg_w, bg_h))
    )?;

    // ---- step 3: scan (hole detection + ini lookup)
    crate::println!("step 3: scanning tables (hole detection + ini lookup)")?;
    // the answers are saved now, before the long part: the next wizard run
    // starts from them, and an interrupted scan is not starting over
    let vpx = detect_vpx_executable(&ini_path);
    crate::config::set_b2s_answers(&crate::config::B2sAnswersInput {
        ini: Some(&ini_path),
        screenres: screenres.as_deref(),
        playfield_w: Some(playfield_w as i64),
        scaling: Some(scaling as i64),
        monitor: Some(&rect_string(&geometry.monitor(bg_w, bg_h))),
        vpx_executable: vpx.as_deref(),
    })?;
    crate::println!("  answers saved to the vpxtool config")?;
    let records = super::scan::scan_quiet(ScanArgs {
        target: args.target.clone(),
        ini: Some(ini_path.clone()),
        method: super::Method::Auto,
        no_hole: false,
        json: None,
        write: false,
        backup: false,
        force: false,
        glass_roughness: 0.0,
        aspect: super::Aspect::Hole,
        native_grid: None,
        backglass_monitor: geometry.monitor(bg_w, bg_h),
        limit: None,
        jobs: args.jobs,
        ini_extra: Vec::new(),
    })
    .map_err(io::Error::other)?;
    let pairs = records
        .iter()
        .filter(|r| r.hole_backglass.is_some() && r.ini_rect.is_some())
        .count();
    crate::println!("  {pairs} tables carry a detected window and an ini section")?;

    // ---- step 4: the candidate from the screen geometry, corrected by the
    // tables' own data: the median signed offset of the mapped ini rect to
    // the detected window is where the candidate is wrong, in desktop pixels
    let mut monitor = geometry.monitor(bg_w, bg_h);
    crate::println!("step 4: verifying against the tables' own data")?;
    crate::println!("  from the screen geometry: {}", rect_string(&monitor))?;
    if pairs == 0 {
        crate::println!("  no table has both sources, the candidate is unverified")?;
    }
    // the sizes are verified too: each table's ini window, projected onto
    // its painted window, says how wide the monitor is in the ini's space -
    // that is the scaled size, and on a machine with mixed display scaling
    // the display settings show something else
    if let Some((ew, eh)) = estimated_monitor_size(&records) {
        crate::println!(
            "  the tables imply a backglass monitor of {:.0}x{:.0} in the ini's space (the answers said {:.0}x{:.0})",
            ew,
            eh,
            monitor.w,
            monitor.h
        )?;
        if (ew - monitor.w).abs() > monitor.w * 0.10 || (eh - monitor.h).abs() > monitor.h * 0.10 {
            crate::println!(
                "  that is the scaling: the display settings report the physical size, the ini lives in the scaled one"
            )?;
            if confirm("  adopt the measured size", true)? {
                monitor.w = ew;
                monitor.h = eh;
                crate::println!("  adopting: {:.0}x{:.0}", ew, eh)?;
            }
        }
    }
    for round in 0..4 {
        // the first rounds run over all pairs, the last ones only over the
        // tables that already agree, to finish precisely
        let tol = (round >= 2).then_some(0.05 * monitor.w);
        let Some(dx) = median_shift(&records, &monitor, true, tol) else {
            break;
        };
        let Some(dy) = median_shift(&records, &monitor, false, tol) else {
            break;
        };
        if dx.abs() < 5.0 && dy.abs() < 5.0 {
            break;
        }
        monitor = Rect::new(monitor.x + dx, monitor.y + dy, monitor.w, monitor.h);
        crate::println!(
            "  the tables say the monitor is {dx:+.0} px right and {dy:+.0} px down: {}",
            rect_string(&monitor)
        )?;
    }
    let (dxs, dys, pairs) = residuals(&records, &monitor);
    if pairs > 0 {
        crate::println!(
            "  median distance of the ini rect to the detected window: {:.0} px in x, {:.0} px in y on {pairs} tables",
            median(&dxs),
            median(&dys)
        )?;
        let far = dxs.iter().filter(|d| **d > 0.05 * monitor.w).count();
        if far > 0 {
            crate::println!(
                "  {far} table(s) are further away: their virtual DMD window sits off the painted cut-out, the ini method stamps those at the window position"
            )?;
        }
        if median(&dxs) > 0.05 * monitor.w {
            crate::println!("  the median is large: check the ini path and the screen sizes")?;
        }
    }

    // ---- step 5: confirm and write
    let answer = prompt_line(
        "  accept (y/n, or type x,y,width,height)",
        Some("y"),
        interactive,
    )?;
    let monitor = if answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes") {
        monitor
    } else {
        answer
            .parse::<Rect>()
            .map_err(|e| io::Error::other(format!("not a monitor rectangle {answer:?}: {e}")))?
    };
    if !confirm("write it to the vpxtool config file", interactive)? {
        crate::println!(
            "  not written; pass it per run with: --monitor {}",
            rect_string(&monitor)
        )?;
        return Ok(());
    }
    let path = crate::config::set_b2s_answers(&crate::config::B2sAnswersInput {
        monitor: Some(&rect_string(&monitor)),
        ..Default::default()
    })?;
    crate::println!(
        "step 5: wrote b2s_backglass_monitor = \"{}\" to {}",
        rect_string(&monitor),
        path.display()
    )?;
    crate::println!(
        "done. `b2s stampdmd --method ini` now maps the ini windows onto the detected DMD windows."
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_setup_builds_the_monitor_from_the_screenres_numbers() {
        // the starting point: the backglass right of a 2560 wide playfield,
        // flush with the top of the desktop; the placement Windows gives the
        // monitor is what the tables then measure
        let g = Geometry { x: 2560.0, y: 0.0 };
        assert_eq!(
            g.monitor(2560.0, 2880.0),
            Rect::new(2560.0, 0.0, 2560.0, 2880.0)
        );
    }

    #[test]
    fn screenres_numbers_are_read_past_comments_and_odd_tokens() {
        let dir = std::env::temp_dir().join("vpxtool-screenres-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ScreenRes.txt");
        std::fs::write(
            &path,
            "#Playfield resolution\n2560 1440\n100% 100%\n#Select backglass monitor\n=2\n@0 0\n1280 331\n#glowworm\n\\\\.\\DISPLAY3\n",
        )
        .unwrap();
        let info = read_screenres(&path).unwrap();
        assert_eq!(info.playfield_w, 2560.0);
        assert_eq!(info.playfield_h, 1440.0);
        // the old format has no backglass section - no pixels to trust there
        assert_eq!(info.backglass, None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_tables_imply_the_monitor_size_from_the_ini_window_over_the_hole() {
        // image 1621x1731, ini window 1344x318, painted window 844x210:
        // the monitor is 1621*1344/844 = 2583 px wide in the ini's space -
        // the 150% scaled size of a 1707 px panel, bezel included
        let records: Vec<Record> = (0..8)
            .map(|_| Record {
                hole_backglass: Some(Rect::new(476.0, 1568.0, 844.0, 210.0)),
                ini_rect: Some(Rect::new(4445.0, 2412.0, 1344.0, 318.0)),
                backglass_size: Some((1621, 1731)),
                ..Default::default()
            })
            .collect();
        let (w, _h) = estimated_monitor_size(&records).unwrap();
        // 1621 * 1344 / 844 = 2581.3
        assert!((w - 2581.3).abs() < 0.1, "w was {w}");
    }

    #[test]
    fn the_v6_screenres_has_the_backglass_size_in_the_ini_space() {
        let dir = std::env::temp_dir().join("vpxtool-screenres-v6-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ScreenRes.txt");
        std::fs::write(
            &path,
            "# V6.0.0\n# Playfield Screen resolution width/height\n2560\n1440\n             # Backglass width/height\n2560\n2880\n             # Select backglass screen\n=2\n# Backglass x/y position\n0\n0\n             # width/height of the B2S (or Full) DMD area\n1280\n960\n",
        )
        .unwrap();
        let info = read_screenres(&path).unwrap();
        assert_eq!(info.playfield_w, 2560.0);
        assert_eq!(info.playfield_h, 1440.0);
        // the DualUp panel at 150%: B2S says 2560x2880, and that is the
        // size the ini coordinates live in
        assert_eq!(info.backglass, Some((2560.0, 2880.0)));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_position_is_x_comma_y() {
        assert_eq!(parse_xy("3840,0"), Some((3840.0, 0.0)));
        assert_eq!(parse_xy(" 1283 , -846 "), Some((1283.0, -846.0)));
        assert_eq!(parse_xy("3840"), None);
        assert_eq!(parse_xy("a,b"), None);
    }

    fn ini_fixture(dir: &Path, name: &str, sections: &str) -> DmdDeviceIni {
        let path = dir.join(name);
        std::fs::write(&path, sections).unwrap();
        DmdDeviceIni::load(&path).unwrap()
    }

    const TWO_WINDOWS: &str = "[a]
virtualdmd left=2700
virtualdmd top=100
             virtualdmd width=1000
virtualdmd height=200
             [b]
virtualdmd left=4300
virtualdmd top=2500
             virtualdmd width=800
virtualdmd height=300
";

    #[test]
    fn ini_windows_that_do_not_fit_the_monitor_are_a_problem() {
        let dir = std::env::temp_dir().join("vpxtool-sanity-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ini = ini_fixture(&dir, "two.ini", TWO_WINDOWS);
        // x 2700..5100, y 100..2800: a monitor at 2560,0,2560,2880 holds it
        let ok = sanity_problems(2560.0, 0.0, 2560.0, 2880.0, 2560.0, &ini, None);
        assert!(!ok.iter().any(|p| p.contains("do not fit")), "{ok:?}");
        // a two section ini is fishy on its own
        assert!(ok.iter().any(|p| p.contains("only 2 sections")));
        // at 0,0 the windows hang past both edges; the two-section ini is
        // below the majority check's floor, so this one is a small fixture
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_ini_window_majority_judges_the_monitor() {
        let dir = std::env::temp_dir().join("vpxtool-sanity-majority-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // nine windows on the monitor at 2560,0,2560,2880, one dragged away
        let many = (0..9)
            .map(|i| {
                format!(
                    "[r{i}]
virtualdmd left=2700
virtualdmd top=100
                     virtualdmd width=1000
virtualdmd height=200
"
                )
            })
            .collect::<String>()
            + "[far]
virtualdmd left=9000
virtualdmd top=100
               virtualdmd width=1000
virtualdmd height=200
";
        let ini = ini_fixture(&dir, "many.ini", &many);
        let ok = sanity_problems(2560.0, 0.0, 2560.0, 2880.0, 2560.0, &ini, None);
        assert!(!ok.iter().any(|p| p.contains("fall outside")), "{ok:?}");
        let bad = sanity_problems(0.0, 0.0, 2560.0, 1440.0, 2560.0, &ini, None);
        assert!(bad.iter().any(|p| p.contains("10 of 10")), "{bad:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn answers_that_disagree_with_the_screenres_are_a_problem() {
        let dir = std::env::temp_dir().join("vpxtool-sanity-stale-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let many = (0..6)
            .map(|i| {
                format!(
                    "[r{i}]
virtualdmd left=2700
virtualdmd top=100
                     virtualdmd width=1000
virtualdmd height=200
"
                )
            })
            .collect::<String>();
        let ini = ini_fixture(&dir, "many.ini", &many);
        let sr = ScreenResInfo {
            playfield_w: 2560.0,
            playfield_h: 1440.0,
            backglass: Some((2560.0, 2880.0)),
        };
        // the physical panel size, not the ini's size: more than 20% off
        let problems = sanity_problems(2560.0, 0.0, 1707.0, 1920.0, 2560.0, &ini, Some(sr));
        assert!(
            problems.iter().any(|p| p.contains("ScreenRes")),
            "{problems:?}"
        );
        // agreeing is silent
        let quiet = sanity_problems(2560.0, 0.0, 2560.0, 2880.0, 2560.0, &ini, Some(sr));
        assert!(!quiet.iter().any(|p| p.contains("ScreenRes")), "{quiet:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_short_screenres_is_an_error_not_a_guess() {
        let dir = std::env::temp_dir().join("vpxtool-screenres-short-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ScreenRes.txt");
        std::fs::write(&path, "# nothing\n2560\n").unwrap();
        assert!(read_screenres(&path).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
