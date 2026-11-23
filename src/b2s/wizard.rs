//! The calibration wizard: `vpxtool b2s wizard <TABLES>`.
//!
//! The ini method maps the desktop coordinates of DmdDevice.ini into the
//! backglass image, and that needs the backglass monitor rectangle: where it
//! sits on the desktop right of the playfield. vpxtool's default
//! 2560,0,2560,2880 is a guess; on a 4K playfield it slides every stamp by
//! over a thousand pixels.
//!
//! The rectangle is the B2S backglass window, and the default screen setup
//! (the B2S wiki's "Rearrange your displays" picture) builds it from three
//! numbers: the playfield size, and the height of the B2S DMD area that the
//! playfield shows at its bottom - ScreenRes.txt line 1 (playfield width),
//! line 2 (height), line 10 ([dmd] height):
//!
//! ```text
//! x0 = playfield width                   x1 = x0 + backglass width
//! y0 = playfield height - dmd height     y1 = y0 + backglass height
//! ```
//!
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

/// The screen geometry as the default B2S setup builds it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    pub playfield_w: f64,
    pub playfield_h: f64,
    pub dmd_area_h: f64,
}

impl Geometry {
    pub fn monitor(&self, backglass_w: f64, backglass_h: f64) -> Rect {
        // the bottom edge of the backglass window sits as far below the
        // playfield as the DMD strip is tall, so the strip lands under the
        // playfield and the rest of the backglass rises over the desktop
        let y1 = self.playfield_h + self.dmd_area_h;
        Rect::new(self.playfield_w, y1 - backglass_h, backglass_w, backglass_h)
    }
}

/// The DMD strip question, spelled out: the term comes from the B2S
/// installation, a user meets it in ScreenRes.txt and in the B2S Display
/// Setup dialog, and the connection to the backglass position is the whole
/// point of asking.
const DMD_AREA_QUESTION: &str = "  the DMD area: the strip at the bottom of the backglass image from which B2S renders the DMD, placed below the playfield\n    (ScreenRes.txt: \"width/height of the B2S (or Full) DMD area\", the second number; default 550)\n  area height (pixels)";

/// The default B2S DMD area height in ScreenRes.txt.
const DEFAULT_DMD_AREA_H: f64 = 550.0;

/// Reads ScreenRes.txt (the format is one value per line, comments start
/// with `#`, lines end in CRLF, `%` values are not plain numbers): the
/// playfield width and height are the first two numbers, the height of the
/// B2S DMD area is the number right after the DMD area width. The width is
/// found as the first number after the backglass line that is a plausible
/// DMD area width: at least a third of the playfield width and not more
/// than it - the layout lines (the `0 0` positions, the `0` of the Y-flip)
/// must not be mistaken for it.
pub fn read_screenres(path: &Path) -> io::Result<Geometry> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| io::Error::other(format!("read {}: {e}", path.display())))?;
    let mut numbers: Vec<f64> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        for token in line.split_whitespace() {
            if let Ok(n) = token.parse::<f64>() {
                numbers.push(n);
            }
        }
    }
    if numbers.len() < 2 {
        return Err(io::Error::other(
            "ScreenRes.txt: not enough numbers to find the playfield size",
        ));
    }
    let (playfield_w, playfield_h) = (numbers[0], numbers[1]);
    let dmd_area_h = numbers
        .iter()
        .skip(2)
        .enumerate()
        .find(|(_, w)| **w >= playfield_w / 3.0 && **w <= playfield_w)
        .and_then(|(i, _)| numbers.get(i + 3)) // +3: 1-based shift of skip(2), then the height follows the width
        .copied()
        .unwrap_or(DEFAULT_DMD_AREA_H);
    Ok(Geometry {
        playfield_w,
        playfield_h,
        dmd_area_h,
    })
}

/// Asks a question on a terminal; `default` is taken on an empty answer and
/// in non-interactive mode.
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

    // ---- step 1: DmdDevice.ini
    crate::println!("step 1: DmdDevice.ini")?;
    let found = args.ini.clone().unwrap_or_else(|| super::default_ini(&[]));
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
    // ScreenRes.txt is a hint, not the answer: it says how big the playfield
    // is and how high the DMD area is, and the default arrangement of the
    // B2S wiki turns those into the backglass window - x0 = playfield width,
    // y1 = playfield height + DMD area height. The Windows monitor placement can
    // say otherwise (a backglass monitor placed flush to the top of the
    // desktop has y0 = 0), so the candidate is verified against the tables
    // in step 4, and their own data corrects it.
    crate::println!(
        "step 2: the screen setup (default B2S arrangement: backglass right of the playfield, its bottom reaching dmd area height below the playfield)"
    )?;
    let screenres = match &args.target {
        Target::Directory(d) => Some(d.join("ScreenRes.txt")),
        _ => None,
    }
    .filter(|p| p.exists());
    let geometry = match &screenres {
        Some(path) => {
            let g = read_screenres(path)?;
            crate::println!(
                "  ScreenRes.txt: playfield {:.0}x{:.0}, DMD area height {:.0}",
                g.playfield_w,
                g.playfield_h,
                g.dmd_area_h
            )?;
            // the file can be stale - a resolution change is one click in the
            // Windows settings, ScreenRes.txt is only rewritten by the B2S
            // setup -, so the parsed numbers are the defaults, not the answer
            Geometry {
                playfield_w: prompt_number(
                    "  playfield width (pixels)",
                    g.playfield_w,
                    interactive,
                )?,
                playfield_h: prompt_number(
                    "  playfield height (pixels)",
                    g.playfield_h,
                    interactive,
                )?,
                dmd_area_h: prompt_number(DMD_AREA_QUESTION, g.dmd_area_h, interactive)?,
            }
        }
        None => {
            crate::println!("  no ScreenRes.txt found in the table folder")?;
            Geometry {
                playfield_w: prompt_number("  playfield width (pixels)", 2560.0, interactive)?,
                playfield_h: prompt_number("  playfield height (pixels)", 1440.0, interactive)?,
                dmd_area_h: prompt_number(DMD_AREA_QUESTION, 550.0, interactive)?,
            }
        }
    };
    let bg_w = prompt_number("  backglass screen width (pixels)", 2560.0, interactive)?;
    let bg_h = prompt_number("  backglass screen height (pixels)", 2880.0, interactive)?;

    // ---- step 3: scan (hole detection + ini lookup)
    crate::println!("step 3: scanning tables (hole detection + ini lookup)")?;
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
    let vpx = detect_vpx_executable(&ini_path);
    if let Some(exe) = &vpx {
        crate::println!("  vpx_executable for the config: {}", exe.display())?;
    }
    let path = crate::config::set_b2s_backglass_monitor(&rect_string(&monitor), vpx.as_deref())?;
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
        // a 2560x1440 playfield, the 550 DMD area, a 2560x2880 backglass:
        // the wiki picture's layout. The bottom edge is 1440+550, the top
        // edge is 2880 above that: -890, a monitor reaching over the top of
        // the desktop, measured on a real cabinet as -846
        let g = Geometry {
            playfield_w: 2560.0,
            playfield_h: 1440.0,
            dmd_area_h: 550.0,
        };
        assert_eq!(
            g.monitor(2560.0, 2880.0),
            Rect::new(2560.0, -890.0, 2560.0, 2880.0)
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
        let g = read_screenres(&path).unwrap();
        assert_eq!(g.playfield_w, 2560.0);
        assert_eq!(g.playfield_h, 1440.0);
        // the number after the DMD area width 1280 is its height, 331 here
        assert_eq!(g.dmd_area_h, 331.0);
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
