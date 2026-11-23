//! The calibration wizard: `vpxtool b2s wizard <TABLES>`.
//!
//! The ini method maps the desktop coordinates of DmdDevice.ini into the
//! backglass image, and that needs the backglass monitor rectangle: where it
//! sits on the desktop right of the playfield. vpxtool's default
//! 2560,0,2560,2880 is a guess; on a 4K playfield it slides every stamp by
//! over a thousand pixels.
//!
//! Where that rectangle is cannot be read off ScreenRes.txt: the file says
//! how big the playfield window is and how big the B2S DMD area is - the
//! area on the backglass screen from which B2S renders the DMD, the size of
//! the grill window on the monitor, the grill being the painted strip the
//! DMD shows through - but the position of the backglass monitor on the
//! desktop is Windows display placement, free to be anything, negative y
//! included. ScreenRes.txt only says the B2S window sits at (0,0) of the
//! screen it selects.
//!
//! So the wizard takes the numbers it can read as the starting point -
//! x0 = playfield width (the default arrangement puts the backglass right
//! of the playfield), y0 = 0, size = the backglass screen - and lets the
//! tables themselves measure the rest.
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

/// One monitor, as Windows reports it.
#[derive(Clone, Debug, PartialEq)]
pub struct Monitor {
    pub name: String,
    pub rect: Rect,
}

/// The monitors as Windows reports them to this process. vpxtool carries no
/// DPI awareness manifest, so a plain build sees the desktop the way the
/// classic Win32 programs see it - the space of the `DmdDevice.ini`
/// coordinates - as long as no compat flags shift a program elsewhere.
/// Empty off Windows.
pub fn windows_monitors() -> Vec<Monitor> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Graphics::Gdi::{
            EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW,
        };
        unsafe extern "system" fn collect(
            hmon: HMONITOR,
            _hdc: HDC,
            _rc: *mut windows_sys::Win32::Foundation::RECT,
            data: isize,
        ) -> i32 {
            let out = &mut *(data as *mut Vec<Monitor>);
            let mut mi: MONITORINFOEXW = unsafe { std::mem::zeroed() };
            mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
            if GetMonitorInfoW(hmon, &mut mi.monitorInfo) != 0 {
                let rc = mi.monitorInfo.rcMonitor;
                let end = mi
                    .szDevice
                    .iter()
                    .position(|c| *c == 0)
                    .unwrap_or(mi.szDevice.len());
                out.push(Monitor {
                    name: String::from_utf16_lossy(&mi.szDevice[..end]),
                    rect: Rect::new(
                        rc.left as f64,
                        rc.top as f64,
                        (rc.right - rc.left) as f64,
                        (rc.bottom - rc.top) as f64,
                    ),
                });
            }
            1
        }
        unsafe {
            let mut out: Vec<Monitor> = Vec::new();
            EnumDisplayMonitors(
                std::ptr::null_mut(),
                std::ptr::null(),
                Some(collect),
                &mut out as *mut Vec<Monitor> as isize,
            );
            out
        }
    }
    #[cfg(not(windows))]
    Vec::new()
}

/// The wizard defaults from the monitors: the primary monitor (the one
/// holding the desktop origin) is the playfield, the largest of the rest the
/// backglass.
fn default_from_monitors(monitors: &[Monitor]) -> Option<(f64, (f64, f64))> {
    let primary = monitors.iter().find(|m| {
        m.rect.x <= 0.0
            && m.rect.x + m.rect.w >= 1.0
            && m.rect.y <= 0.0
            && m.rect.y + m.rect.h >= 1.0
    })?;
    let bg = monitors
        .iter()
        .filter(|m| m.name != primary.name)
        .max_by_key(|m| (m.rect.w * m.rect.h) as i64)?;
    Some((primary.rect.w, (bg.rect.w, bg.rect.h)))
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
    pub playfield_w: f64,
}

impl Geometry {
    pub fn monitor(&self, backglass_w: f64, backglass_h: f64) -> Rect {
        Rect::new(self.playfield_w, 0.0, backglass_w, backglass_h)
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
        "step 2: the screen setup (the default arrangement puts the backglass window right of the playfield)"
    )?;
    crate::println!(
        "  the position of the backglass monitor on the desktop is Windows display placement, no file knows it - the tables measure it in step 4"
    )?;
    let screenres = match &args.target {
        Target::Directory(d) => Some(d.join("ScreenRes.txt")),
        _ => None,
    }
    .filter(|p| p.exists());
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
        None => {
            crate::println!("  no ScreenRes.txt found in the table folder")?;
            None
        }
    };
    // the file can be stale - a resolution change is one click in the
    // Windows settings, ScreenRes.txt is only rewritten by the B2S setup -,
    // so its numbers are the defaults of the questions, not the answers
    // on Windows, the monitor list as the ini's space sees it is the best
    // default there is
    let monitors = windows_monitors();
    if !monitors.is_empty() {
        crate::println!("  monitors as windows reports them to this process:")?;
        for m in &monitors {
            crate::println!(
                "    {}  {:.0},{:.0}  {:.0}x{:.0}",
                m.name,
                m.rect.x,
                m.rect.y,
                m.rect.w,
                m.rect.h
            )?;
        }
    }
    let mon = default_from_monitors(&monitors);
    let playfield_w = prompt_number(
        "  playfield screen width (pixels): the backglass window starts at this x",
        screenres_info
            .map(|i| i.playfield_w)
            .or_else(|| mon.map(|d| d.0))
            .unwrap_or(2560.0),
        interactive,
    )?;
    let geometry = Geometry { playfield_w };
    let bg_w = prompt_number(
        "  backglass screen width (pixels)",
        mon.map(|d| d.1.0).unwrap_or(2560.0),
        interactive,
    )?;
    let bg_h = prompt_number(
        "  backglass screen height (pixels)",
        mon.map(|d| d.1.1).unwrap_or(2880.0),
        interactive,
    )?;

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
        // the starting point: the backglass right of a 2560 wide playfield,
        // flush with the top of the desktop; the placement Windows gives the
        // monitor is what the tables then measure
        let g = Geometry {
            playfield_w: 2560.0,
        };
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
    fn the_monitors_give_the_defaults_primary_is_the_playfield() {
        let monitors = vec![
            Monitor {
                name: "\\\\.\\DISPLAY1".into(),
                rect: Rect::new(2560.0, 0.0, 1707.0, 1920.0),
            },
            Monitor {
                name: "\\\\.\\DISPLAY2".into(),
                rect: Rect::new(0.0, 0.0, 2560.0, 1440.0),
            },
        ];
        // the playfield width default is the primary, the backglass size the
        // largest of the rest
        assert_eq!(
            default_from_monitors(&monitors),
            Some((2560.0, (1707.0, 1920.0)))
        );
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
    fn a_short_screenres_is_an_error_not_a_guess() {
        let dir = std::env::temp_dir().join("vpxtool-screenres-short-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ScreenRes.txt");
        std::fs::write(&path, "# nothing\n2560\n").unwrap();
        assert!(read_screenres(&path).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
