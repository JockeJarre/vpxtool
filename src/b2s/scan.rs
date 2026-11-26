//! Collects the tables of a folder tree, matches them to their B2S backglass
//! file, and works out the DMD rectangle to stamp.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};

use regex::Regex;
use serde::Serialize;

use crate::b2s::dmdini::DmdDeviceIni;
use crate::b2s::hole;
use crate::b2s::report;
use crate::b2s::rom;
use crate::b2s::stamp;
use crate::b2s::xml::{self as b2s, B2sFile};
use crate::b2s::{Aspect, Grid, IniTarget, Method, Rect, default_ini};

/// The extensions that hold the backglass XML. The real world is messy: `.B2SPro`
/// and `.directb2sPRO` turn up in the same folder as `.directb2s`, and
/// `.directb2st` / `.directb2sss` / `.directb2s_no` look like typos, leftovers
/// of split packs, or backup copies. Only the two documented extensions are
/// candidates; a table that has nothing but a `.directb2st` is reported as
/// missing, because writing the wrong file is worse than doing nothing.
const EXTENSIONS: &[&str] = &["directb2s", "b2spro"];

/// What to stamp: a folder tree of tables, one `.vpx`, or one
/// `.directb2s` / `.b2spro` file.
#[derive(Debug, Clone)]
pub enum Target {
    /// folder tree to scan for *.vpx
    Directory(PathBuf),
    /// a single .vpx, its backglass is found next to it
    Table(PathBuf),
    /// a single .directb2s/.b2spro, its table is found next to it
    B2s(PathBuf),
}

/// The options of `vpxtool b2s stampdmd`, assembled by the cli layer.
#[derive(Debug, Clone)]
pub struct ScanArgs {
    pub target: Target,

    /// path of DmdDevice.ini, defaults to the standard VPinball locations
    pub ini: Option<PathBuf>,

    /// how to derive the rectangle
    pub method: Method,

    /// skip hole detection: no image decoding, so ini-only and much faster
    pub no_hole: bool,

    /// write the collected data as JSON for analysis
    pub json: Option<PathBuf>,

    /// write the DMD element into the .directb2s/.b2spro file (in place)
    pub write: bool,

    /// keep a .bak copy of every file that is written
    pub backup: bool,

    /// overwrite a file that already carries a `<DMD>` tag; without it those
    /// files are left alone
    pub force: bool,

    /// GlassRoughness value to write
    pub glass_roughness: f64,

    /// aspect ratio of the stamped rectangle: keep the hole, keep the ini
    /// window's ratio, or derive the height from `native_grid`. Every authored
    /// tag on a reference installation is 4:1 (one 4.74), so
    /// `--native-grid 4x1` reproduces the authoring convention
    pub aspect: Aspect,

    /// the rom's dot grid, needed by `aspect: Native`: `128x32` for the WPC
    /// era, `192x64` for SAM, and `4x1` for the plain 4:1 content box the
    /// authoring tools draw. Resolving it per rom needs the rom database, which
    /// this tool does not depend on, so it stays an option.
    pub native_grid: Option<Grid>,

    /// the backglass monitor in desktop pixels: the ini window is in
    /// desktop pixels, the `<DMD>` tag is in backglass image pixels, and the
    /// image covers this monitor
    pub backglass_monitor: Rect,

    /// stop after N tables (folder scans)
    pub limit: Option<usize>,

    /// how many tables to read at a time. The work per table is decoding the
    /// embedded images and parsing 20-50 MB of XML, so more than one core pays
    pub jobs: usize,

    /// extra places to look for DmdDevice.ini, e.g. the configured pinmame
    /// folder, tried before the standard locations
    pub ini_extra: Vec<PathBuf>,

    /// which window the ini's DMD rectangles are placed on. `ScoreView` maps
    /// them through the ScreenRes.txt DMD area instead of the monitor
    pub ini_target: super::IniTarget,

    /// ScreenRes.txt data, needed to map ScoreView-based ini windows
    pub screenres: Option<super::wizard::ScreenResInfo>,
}

pub fn default_jobs() -> usize {
    std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1)
}

/// What the user asked for, as one bundle for the per-table pass
#[derive(Clone, Copy, Debug)]
struct Policy {
    method: Method,
    aspect: Aspect,
    /// ratio of `--native-grid`, resolved at startup
    native: Option<f64>,
    /// backglass monitor, desktop pixels
    backglass: Rect,
    want_images: bool,
    ini_target: super::IniTarget,
    screenres: Option<super::wizard::ScreenResInfo>,
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MatchKind {
    Exact,
    Similar,
    Ambiguous,
    #[default]
    Missing,
}

impl std::fmt::Display for MatchKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            MatchKind::Exact => "exact",
            MatchKind::Similar => "similar",
            MatchKind::Ambiguous => "ambiguous",
            MatchKind::Missing => "none",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Default, Serialize)]
pub struct Record {
    pub vpx: String,
    pub b2s: Option<String>,
    /// the same path as a `PathBuf`, for the writer
    #[serde(skip)]
    pub b2s_path: Option<PathBuf>,
    pub match_kind: MatchKind,
    pub rom: Option<String>,
    pub rom_source: Option<String>,
    pub ini_section: bool,
    /// raw ini window, desktop pixels; not in the JSON: `ini_px` carries it
    /// in the comparable form
    #[serde(skip_serializing, default)]
    pub ini_rect: Option<Rect>,
    /// the ini window as `"x,y,w,h"` desktop px, plus the same window
    /// converted to backglass image pixels in parentheses:
    /// `"1283,-846,2560,2880 (760,2320,1475,376)"`. Every other rectangle in
    /// a record is backglass image pixels, so all of them can be compared.
    #[serde(default)]
    pub ini_px: Option<String>,
    pub existing_dmd: Option<Rect>,
    /// every `<DMD Target=...>` the file carries, as (target, rect). The
    /// FullDMD backglasses of the ScoreView era carry two of them
    #[serde(default)]
    pub existing_tags: Vec<(String, Rect)>,
    /// the file has a `DMDImage` element: the FullDMD canvas, the prerequisite
    /// for a Target="ScoreView" stamp
    #[serde(default)]
    pub has_dmd_image: bool,
    pub grill_height: Option<f64>,
    pub backglass_size: Option<(u32, u32)>,
    pub dmd_image_size: Option<(u32, u32)>,
    pub hole_backglass: Option<Rect>,
    pub hole_dmd_image: Option<Rect>,
    /// the stamp rectangle for `Target="ScoreView"`, in the only space it
    /// lives in: DMDImage canvas pixels, the top left of the DMDImage is
    /// 0,0 - never backglass pixels
    #[serde(default)]
    pub scoreview_rect: Option<Rect>,
    /// flat rectangles the tracer saw in the backglass image, before the
    /// DMD-shape filter. Used to stop the template guessing when the image
    /// already contains a stripe that is not a DMD window.
    pub hole_seen: usize,
    /// whether the backglass image was actually inspected
    pub hole_scanned: bool,
    pub chosen: Option<Rect>,
    /// which source `chosen` came from: `hole`, `ini`, `tag` or `template`
    pub source: Option<String>,
    pub method: Method,
    pub aspect: Aspect,
    /// the ini window mapped into backglass image pixels
    pub ini_in_image: Option<Rect>,
    pub notes: Vec<String>,
}

/// The scan without the report and the stamp pass: what the wizard wants.
pub fn scan_quiet(args: ScanArgs) -> Result<Vec<Record>, String> {
    scan_impl(args, true)
}

/// Scans the target, prints the report and runs the stamp pass.
pub fn scan(args: ScanArgs) -> Result<Vec<Record>, String> {
    scan_impl(args, false)
}

fn scan_impl(args: ScanArgs, quiet: bool) -> Result<Vec<Record>, String> {
    let ini_path = args
        .ini
        .clone()
        .unwrap_or_else(|| default_ini(&args.ini_extra));
    let ini = match DmdDeviceIni::load(&ini_path) {
        Ok(ini) => {
            let _ = crate::eprintln!(
                "DmdDevice.ini: {} ({} sections)",
                ini_path.display(),
                ini.section_count()
            );
            Some(ini)
        }
        Err(e) => {
            let _ = crate::eprintln!("warning: {e}, ini lookups disabled");
            None
        }
    };

    let native = match args.aspect {
        Aspect::Native => match args.native_grid {
            Some(grid) => Some(grid.aspect()),
            None => {
                return Err(
                    "--aspect native needs --native-grid, e.g. `--native-grid 128x32`".into(),
                );
            }
        },
        _ => None,
    };
    let policy = Policy {
        method: args.method,
        aspect: args.aspect,
        native,
        backglass: args.backglass_monitor,
        want_images: !args.no_hole,
        ini_target: args.ini_target,
        screenres: args.screenres,
    };

    let work: Vec<Work> = match &args.target {
        Target::Directory(root) => {
            let tables = collect(root, args.limit)?;
            let _ = crate::eprintln!("found {} tables under {}", tables.len(), root.display());
            let mut out: Vec<Work> = tables
                .into_iter()
                .map(|(dir, name)| Work::Table { dir, name })
                .collect();
            // a folder of downloaded backglasses has no tables at all; scan
            // the loose .directb2s/.b2spro files too, hole detection works
            // from the file alone
            let solo = collect_solo(root, args.limit.map(|n| n.saturating_sub(out.len())));
            if !solo.is_empty() {
                let _ = crate::eprintln!(
                    "found {} backglasses without a table under {}",
                    solo.len(),
                    root.display()
                );
                out.extend(solo.into_iter().map(|path| Work::Solo { path }));
            }
            out
        }
        Target::Table(vpx) => {
            let dir = vpx.parent().ok_or("table path without a folder")?;
            let name = vpx.file_name().ok_or("table path without a file name")?;
            vec![Work::Table {
                dir: dir.to_path_buf(),
                name: name.to_string_lossy().into_owned(),
            }]
        }
        Target::B2s(b2s) => {
            let dir = b2s.parent().ok_or("b2s path without a folder")?;
            let stem = b2s.file_stem().ok_or("b2s path without a file name")?;
            match find_vpx(dir, &stem.to_string_lossy()) {
                Some(vpx) => vec![Work::Table {
                    dir: vpx.parent().unwrap().to_path_buf(),
                    name: vpx.file_name().unwrap().to_string_lossy().into_owned(),
                }],
                // a backglass without a table: no rom name, no ini lookup,
                // the hole detection works from the file alone
                None => vec![Work::Solo { path: b2s.clone() }],
            }
        }
    };

    let jobs = args.jobs.max(1);
    let started = std::time::Instant::now();
    let total = work.len();
    // The work per table is decoding the embedded images and parsing 20-50 MB of
    // XML, and the peak of that is the XML of one file, so the workers share a
    // budget of in-flight megabytes: it is what keeps 12 cores from asking for
    // 100 GB at once.
    let budget = Budget::default();
    let mut records = if jobs == 1 || total < 2 {
        let mut out = Vec::with_capacity(total);
        for item in &work {
            let cost = item.cost();
            budget.claim(cost);
            out.push(match item {
                Work::Table { dir, name } => scan_catch(dir, name, &ini, policy),
                Work::Solo { path } => scan_b2s_only(path, &ini, policy),
            });
            budget.release(cost);
            report_progress(out.len(), total);
        }
        out
    } else {
        // a pool over one shared index counter, so a slow table only costs the
        // worker that picked it
        let next = AtomicUsize::new(0);
        let done = AtomicUsize::new(0);
        let out = Mutex::new(Vec::with_capacity(total));
        std::thread::scope(|scope| {
            for _ in 0..jobs.min(total) {
                let next = &next;
                let done = &done;
                let budget = &budget;
                let out = &out;
                let work = &work;
                let ini = &ini;
                scope.spawn(move || {
                    loop {
                        let i = next.fetch_add(1, Ordering::SeqCst);
                        let Some(item) = work.get(i) else {
                            break;
                        };
                        let cost = item.cost();
                        while !budget.claim(cost) {
                            // the budget is held by bigger files, wait for one
                            std::thread::sleep(std::time::Duration::from_millis(50));
                        }
                        let rec = match item {
                            Work::Table { dir, name } => scan_catch(dir, name, ini, policy),
                            Work::Solo { path } => scan_b2s_only(path, ini, policy),
                        };
                        budget.release(cost);
                        out.lock().unwrap().push(rec);
                        report_progress(done.fetch_add(1, Ordering::SeqCst) + 1, total);
                    }
                });
            }
        });
        out.into_inner()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    };
    let _ = crate::eprintln!(
        "{} files in {:.0}s, peak {} MB in flight",
        records.len(),
        started.elapsed().as_secs_f64(),
        budget.peak()
    );
    records.sort_by(|a, b| a.vpx.cmp(&b.vpx));

    if quiet {
        return Ok(records);
    }
    report::print(&records);
    if let Some(path) = &args.json {
        let json = serde_json::to_string_pretty(&records).map_err(|e| e.to_string())?;
        fs::write(path, json).map_err(|e| format!("write {}: {e}", path.display()))?;
        let _ = crate::eprintln!("json written to {}", path.display());
    }
    // the stamp pass: without `write` it only reports the planned edits
    let stats = stamp::stamp(
        &records,
        args.glass_roughness,
        args.write,
        args.backup,
        args.force,
    );
    stamp::print_stats(&stats, args.write);
    Ok(records)
}

/// In-flight megabytes the scan pool may have open at once.
///
/// The cost of a table is dominated by the XML of its backglass file, which is
/// read whole: as bytes and as a `String`, and then again per base64 payload.
/// The estimate comes from the file size, so the budget follows the real files
/// instead of a fixed number of workers, and a folder of 50 MB backglasses does
/// not need 100 GB on 12 cores.
#[derive(Default)]
struct Budget {
    used: AtomicI64,
    peak: AtomicI64,
}

/// The cap, in megabytes. A machine with 32 GB and 14 GB free: the full-library
/// scan measured 2.6 GB at 6 workers, so 6 GB leaves the pool room on 12 cores.
const BUDGET_MB: i64 = 6_144;

impl Budget {
    /// Takes `mb` out of the budget, false when that would go over the cap. One
    /// request is always granted while nothing is in flight, so a file larger
    /// than the whole budget still gets processed.
    fn claim(&self, mb: i64) -> bool {
        let mut used = self.used.load(Ordering::Acquire);
        loop {
            if used > 0 && used + mb > BUDGET_MB {
                return false;
            }
            match self.used.compare_exchange_weak(
                used,
                used + mb,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    self.peak.fetch_max(used + mb, Ordering::Relaxed);
                    return true;
                }
                Err(actual) => used = actual,
            }
        }
    }

    fn release(&self, mb: i64) {
        self.used.fetch_sub(mb, Ordering::AcqRel);
    }

    fn peak(&self) -> i64 {
        self.peak.load(Ordering::Relaxed)
    }
}

/// What one table is expected to cost in flight, in megabytes: the XML as bytes
/// plus as text, the decoded images as RGBA, the `.vpx` script decompressed.
fn estimate(dir: &Path, vpx_name: &str) -> i64 {
    let mb = |bytes: u64| (bytes / 1_048_576).max(1) as i64;
    let stem = Path::new(vpx_name).file_stem().unwrap_or_default();
    let mut out = 8;
    if let Some((path, _)) = find_b2s(dir, &stem.to_string_lossy())
        && let Ok(len) = fs::metadata(&path).map(|m| m.len())
    {
        // bytes + String + the decoded images of one payload
        out += mb(len) * 3;
    }
    if let Ok(len) = fs::metadata(dir.join(vpx_name)).map(|m| m.len()) {
        out += mb(len) / 2;
    }
    out
}

fn report_progress(done: usize, total: usize) {
    if done.is_multiple_of(100) || done == total {
        let _ = crate::eprintln!("  {done}/{total} tables");
    }
}

/// One table, with the panic of a damaged table contained: `vpin` panics on a
/// `.vpx` whose central directory is missing ("the table of contents is not
/// available"), and a folder of 800 tables always has one of those.
fn scan_catch(dir: &Path, vpx_name: &str, ini: &Option<DmdDeviceIni>, policy: Policy) -> Record {
    let run = std::panic::AssertUnwindSafe(|| scan_one(dir, vpx_name, ini, policy));
    match std::panic::catch_unwind(run) {
        Ok(rec) => rec,
        Err(payload) => {
            let msg = payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| payload.downcast_ref::<&str>().copied())
                .unwrap_or("unknown");
            Record {
                vpx: dir.join(vpx_name).display().to_string(),
                notes: vec![format!("read failed: {msg}")],
                ..Default::default()
            }
        }
    }
}

fn scan_one(dir: &Path, vpx_name: &str, ini: &Option<DmdDeviceIni>, policy: Policy) -> Record {
    let vpx = dir.join(vpx_name);
    let stem = vpx
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let mut rec = Record {
        vpx: vpx.display().to_string(),
        match_kind: MatchKind::Missing,
        method: policy.method,
        aspect: policy.aspect,
        ..Default::default()
    };

    let b2s_path = match find_b2s(dir, &stem) {
        Some((path, kind)) => {
            rec.match_kind = kind;
            rec.b2s = Some(path.display().to_string());
            rec.b2s_path = Some(path.clone());
            Some(path)
        }
        None => {
            rec.notes
                .push("no .directb2s/.b2spro next to the table".into());
            None
        }
    };
    let Some(b2s_path) = b2s_path else { return rec };
    analyze(&mut rec, &b2s_path, Some(&vpx), ini, policy);
    rec
}

/// One `.directb2s`/`.b2spro` with no table of a matching name: the rom name
/// and the `DmdDevice.ini` lookup are off the table, everything else - grill,
/// hole detection, the authored tag - works from the file alone.
fn scan_b2s_only(b2s_path: &Path, ini: &Option<DmdDeviceIni>, policy: Policy) -> Record {
    let mut rec = Record {
        vpx: b2s_path.display().to_string(),
        b2s: Some(b2s_path.display().to_string()),
        b2s_path: Some(b2s_path.to_path_buf()),
        match_kind: MatchKind::Exact,
        method: policy.method,
        aspect: policy.aspect,
        ..Default::default()
    };
    rec.notes.push(
        "no .vpx of a matching name: no rom name, the DmdDevice.ini method is unavailable".into(),
    );
    analyze(&mut rec, b2s_path, None, ini, policy);
    rec
}

fn analyze(
    rec: &mut Record,
    b2s_path: &Path,
    vpx: Option<&Path>,
    ini: &Option<DmdDeviceIni>,
    policy: Policy,
) {
    let file: B2sFile = match b2s::probe(b2s_path, policy.want_images) {
        Ok(f) => f,
        Err(e) => {
            rec.notes.push(format!("b2s read failed: {e}"));
            return;
        }
    };
    rec.grill_height = file.grill_height;
    rec.backglass_size = file
        .backglass_image
        .as_ref()
        .and_then(|i| i.size)
        .map(|s| (s.w, s.h));
    rec.dmd_image_size = file
        .dmd_image
        .as_ref()
        .and_then(|i| i.size)
        .map(|s| (s.w, s.h));
    if let Some(tag) = &file.dmd {
        rec.existing_dmd = Some(tag.rect);
    }
    rec.existing_tags = file
        .dmds
        .iter()
        .map(|t| (t.target.clone(), t.rect))
        .collect();
    rec.has_dmd_image = file.images.iter().any(|i| i.element == "DMDImage");

    // ROM name: sidecar .vbs first, then the script embedded in the .vpx
    if let Some(vpx) = vpx {
        match rom::game_name_from_vpx(vpx) {
            Ok(Some(name)) => {
                rec.rom_source = if vpx.with_extension("vbs").exists() {
                    Some("sidecar vbs".into())
                } else {
                    Some("embedded".into())
                };
                rec.rom = Some(name);
            }
            Ok(None) => rec.notes.push("no cGameName in script".into()),
            Err(e) => rec.notes.push(format!("script: {e}")),
        }
    }

    // DmdDevice.ini lookup
    if let (Some(ini), Some(rom)) = (ini, rec.rom.as_deref()) {
        let dmd = ini.lookup(rom);
        rec.ini_section = dmd.section_found;
        rec.ini_rect = dmd.rect();
        if let (Some(window), Some((w, h))) = (rec.ini_rect, rec.backglass_size) {
            rec.ini_in_image = match policy.ini_target {
                IniTarget::Backglass => Some(to_image_space(window, (w, h), policy)),
                IniTarget::ScoreView => match to_scoreview_space(window, policy, (w, h)) {
                    Some(r) => Some(r),
                    None => {
                        rec.notes.push(
                            "no DMD area in ScreenRes.txt: the ini window was mapped as if it sat on the backglass".into(),
                        );
                        Some(to_image_space(window, (w, h), policy))
                    }
                },
            };
        }
        rec.ini_px = rec.ini_rect.map(|r| match rec.ini_in_image {
            Some(m) => format!("{} ({})", r.to_csv(), m.to_csv()),
            None => r.to_csv(),
        });
        if !dmd.section_found {
            rec.notes
                .push(format!("no [{rom}] section in DmdDevice.ini"));
        }
    }

    // hole detection. Only the backglass image is in the coordinate space of the
    // <DMD> tag; a hole in the grill strip (DMDImage) is recorded for diagnosis
    // and never stamped, because its coordinates belong to a different image.
    if policy.want_images {
        if let Some(bytes) = file.image_bytes("BackglassImage") {
            match hole::find_holes_in_bytes(bytes, 1.0 / 3.0) {
                Ok((cands, (w, h))) => {
                    rec.hole_scanned = true;
                    rec.hole_seen = cands.len();
                    let grill = rec.grill_height.filter(|g| *g > 0.0).unwrap_or(0.0);
                    match hole::pick_hole(&cands, w, h, grill) {
                        None => rec.notes.push(format!(
                            "backglass: no hole accepted ({} flat rectangle(s) seen)",
                            cands.len()
                        )),
                        Some(found) => {
                            rec.hole_backglass = Some(found);
                            if grill > 0.0 && hole::pick_in_grill(&[found], h, grill).is_empty() {
                                rec.notes
                                    .push("backglass hole is outside the grill band".to_string());
                            }
                        }
                    }
                }
                Err(e) => rec.notes.push(format!("backglass hole: {e}")),
            }
        }
        if let Some(bytes) = file.image_bytes("DMDImage") {
            match hole::find_holes_in_bytes(bytes, 0.5) {
                Ok((cands, _)) => rec.hole_dmd_image = cands.first().copied(),
                Err(e) => rec.notes.push(format!("dmd image hole: {e}")),
            }
        }
    }

    // The ini coords are desktop pixels; if the backglass monitor passed to
    // `--monitor` starts at the wrong x (a 4K playfield monitor is 3840 wide,
    // not 2560), the mapped rect slides by hundreds of pixels and nobody
    // notices. A detected window that lies far from the mapped ini rect in x
    // is that disagreement, so say it out loud instead of stamping silently.
    if let (Some(h), Some(m), Some((w, _))) =
        (rec.hole_backglass, rec.ini_in_image, rec.backglass_size)
    {
        let dx = (h.x + h.w / 2.0 - (m.x + m.w / 2.0)).abs();
        if dx > 0.10 * w as f64 {
            rec.notes.push(format!(
                "hole and ini rect disagree by {dx:.0} px in x: the ini coords are desktop pixels, check --monitor / config key b2s_backglass_monitor"
            ));
        }
    }

    if let Some((rect, source)) = pick(policy, rec) {
        rec.chosen = Some(rect);
        rec.source = Some(source.to_string());
        if rec.has_dmd_image {
            rec.scoreview_rect = to_dmdimage_space(rec, policy);
        }
        if !has_grill(rec) {
            rec.notes.push(
                "no grill strip: Target=\"Backglass\" omitted, stamped Target=\"ScoreView\" only"
                    .to_string(),
            );
        }
    } else if !has_grill(rec) && !rec.has_dmd_image {
        let grill = rec.grill_height.unwrap_or(0.0);
        if grill <= 0.0 {
            if rec.hole_backglass.is_some() {
                rec.notes
                    .push("no grill strip (GrillHeight 0): hole ignored, left untouched".into());
            } else {
                rec.notes.push(
                    "no grill strip (GrillHeight 0): nothing to stamp, left untouched".into(),
                );
            }
        } else {
            rec.notes.push(format!(
                "grill strip is only {grill:.0} px: no DMD window, left untouched"
            ));
        }
    } else if !has_dmd_evidence(rec) {
        if rec.hole_backglass.is_some() {
            rec.notes.push(
                "flat rectangle found but no DMD evidence (no DMDImage, tag, ini section or rom name): not stamping, left untouched"
                    .into(),
            );
        } else {
            rec.notes.push(
                "no DMD evidence (no DMDImage, tag, ini section or rom name): nothing to stamp, left untouched"
                    .into(),
            );
        }
    } else if policy.method == Method::Template {
        if !rec.hole_scanned {
            rec.notes
                .push("backglass image not inspected: not guessing, left untouched".into());
        } else if rec.hole_seen > 0 {
            rec.notes.push(format!(
                "{} flat rectangle(s) but none DMD-shaped: not guessing, left untouched",
                rec.hole_seen
            ));
        } else if rec.backglass_size.is_none() {
            rec.notes
                .push("no backglass image size: nothing to stamp, left untouched".into());
        }
    }
}

/// A grill strip has to be a real strip. 1-5 px values do occur in the wild
/// (one installation had 1, 2 and 3 px) and pass a literal `GrillHeight > 0`,
/// but no speaker panel is one pixel high: they are leftover or default values.
/// The smallest real grill of that installation is 10 px on a 944 px image,
/// about 1 % of the height, which is the second bound.
pub const MIN_GRILL_PX: f64 = 10.0;
pub const MIN_GRILL_FRAC: f64 = 0.01;

/// The rectangle for `Target="ScoreView"`, in the DMDImage's own space:
/// DMDImage pixels, top left 0,0. A window traced inside the DMDImage is
/// already there. Everything else - a hole in the backglass, the ini
/// mapping - describes the window on the backglass image; the ScoreView
/// window paints the DMDImage over the ScreenRes DMD area, so the spot
/// inside that area, scaled by the DMDImage, is the canvas rectangle.
/// Without a DMD area the image covers the whole backglass.
fn to_dmdimage_space(rec: &Record, policy: Policy) -> Option<Rect> {
    let dmd = rec.dmd_image_size?;
    if let Some(h) = rec.hole_dmd_image {
        return Some(h);
    }
    let rect = rec.chosen?;
    let img = rec.backglass_size?;
    let canvas = policy
        .screenres
        .and_then(|s| {
            let (ax, ay, aw, ah) = s.dmd_area?;
            let (bg_w, bg_h) = s.backglass?;
            if aw <= 0.0 || ah <= 0.0 || bg_w <= 0.0 || bg_h <= 0.0 {
                return None;
            }
            Some(Rect::new(
                ax * img.0 as f64 / bg_w,
                ay * img.1 as f64 / bg_h,
                aw * img.0 as f64 / bg_w,
                ah * img.1 as f64 / bg_h,
            ))
        })
        .unwrap_or(Rect::new(0.0, 0.0, img.0 as f64, img.1 as f64));
    if canvas.w <= 0.0 || canvas.h <= 0.0 {
        return None;
    }
    let sx = dmd.0 as f64 / canvas.w;
    let sy = dmd.1 as f64 / canvas.h;
    Some(Rect::new(
        (rect.x - canvas.x) * sx,
        (rect.y - canvas.y) * sy,
        rect.w * sx,
        rect.h * sy,
    ))
}

/// Which `<DMD>` elements this record's file supports: `Target="Backglass"`
/// over a real grill strip, `Target="ScoreView"` over the FullDMD canvas (a
/// `DMDImage` in the file). The two rectangles live in different spaces:
/// Backglass is backglass image pixels, ScoreView is DMDImage pixels with
/// the DMDImage's own top left as 0,0 - the same window, the canvas it is
/// painted on.
pub fn stamp_targets(rec: &Record) -> Vec<(String, Rect)> {
    let Some(rect) = rec.chosen else {
        return Vec::new();
    };
    let mut targets = Vec::new();
    if has_grill(rec) {
        targets.push(("Backglass".to_string(), rect));
    }
    if rec.has_dmd_image {
        targets.push(("ScoreView".to_string(), rec.scoreview_rect.unwrap_or(rect)));
    }
    targets
}

/// Is there a grill strip that could hold a DMD window?
fn has_grill(rec: &Record) -> bool {
    let Some(g) = rec.grill_height.filter(|g| *g >= MIN_GRILL_PX) else {
        return false;
    };
    match rec.backglass_size {
        Some((_, h)) => g >= MIN_GRILL_FRAC * h as f64,
        // without the image size only the absolute minimum can be checked
        None => true,
    }
}

/// The tag the author wrote, if it is a DMD-shaped rectangle. A placeholder
/// (`0,0 0x0`) or a stripe an older run wrote is not evidence of a DMD.
fn authored_tag(rec: &Record) -> Option<Rect> {
    rec.existing_dmd
        .filter(|r| r.w > 0.0 && r.h > 0.0)
        .filter(|r| match rec.backglass_size {
            Some((w, h)) => hole::is_plausible(*r, w, h),
            // without an image there is nothing to check against; an authored
            // tag is still the author's word
            None => true,
        })
}

/// Does anything say this backglass has a DMD at all? A `DMDImage` or an
/// authored tag is the author's word, an ini section is the user's, and a rom
/// name in the script means the table drives a real DMD machine. Without one
/// of these, a detected rectangle is a flat band of the artwork, not a window:
/// the 8 false stamps of one library had no tag, and most had no DMDImage.
fn has_dmd_evidence(rec: &Record) -> bool {
    rec.ini_section
        || rec.has_dmd_image
        || rec.dmd_image_size.is_some()
        || rec.rom.is_some()
        || authored_tag(rec).is_some()
}

/// The rectangle to stamp and where it came from.
fn pick(policy: Policy, rec: &Record) -> Option<(Rect, &'static str)> {
    // A real grill strip opens the Target="Backglass" door, a DMDImage opens
    // the Target="ScoreView" door; a file with neither gets no stamp at all:
    // without one of them there is no DMD anywhere, and the tracer finds flat
    // bands of the artwork instead.
    if !has_grill(rec) && !rec.has_dmd_image {
        return None;
    }
    // The hole and the template box are the two *derived* rectangles. They are
    // only trusted on a backglass that carries a DMD somewhere (image, authored
    // tag or ini section); otherwise a flat stripe of art looks exactly like a
    // window. The ini rect and the authored tag are evidence by themselves.
    let from_hole = rec.hole_backglass.filter(|_| has_dmd_evidence(rec));
    let from_ini = rec.ini_in_image.or(rec.ini_rect);
    let rect_and_source = match policy.method {
        Method::Ini => from_ini.map(|r| (r, "ini")),
        Method::Hole => from_hole.map(|r| (r, "hole")),
        // A tuned DmdDevice.ini is the most exact source there is: the
        // owner placed that window for this very ROM. The traced hole is a
        // few pixels shy on photo-real backglasses, where the dark window
        // fades into the artwork. So when the two agree on the center, the
        // ini wins; when they disagree, the ini is a foreign window (a
        // default from another machine) and the hole wins.
        Method::Auto => match (from_hole, from_ini) {
            (Some(hole), Some(ini)) => {
                // a DMD window is a wide, short strip; its center is judged
                // against the window's own width, both axes
                let dx = ((hole.x + hole.w / 2.0) - (ini.x + ini.w / 2.0)).abs();
                let dy = ((hole.y + hole.h / 2.0) - (ini.y + ini.h / 2.0)).abs();
                if dx <= 0.15 * hole.w.max(ini.w) && dy <= 0.5 * hole.h {
                    Some((ini, "ini"))
                } else {
                    Some((hole, "hole"))
                }
            }
            (Some(hole), None) => Some((hole, "hole")),
            (None, Some(ini)) => Some((ini, "ini")),
            (None, None) => None,
        },
        // then the rectangle the author already wrote, and only then the box
        // the authoring tools draw. That last resort needs two things: a
        // backglass with DMD evidence, and an image in which the tracer saw no
        // flat rectangle at all. If it did see one but rejected it (wrong
        // shape, wrong place), the image has a stripe that is not a DMD window,
        // so the file is left untouched instead of being given a guessed box.
        Method::Template => from_hole
            .map(|r| (r, "hole"))
            .or_else(|| from_ini.map(|r| (r, "ini")))
            .or_else(|| authored_tag(rec).map(|r| (r, "tag")))
            .or_else(|| {
                if !has_dmd_evidence(rec) || !rec.hole_scanned || rec.hole_seen > 0 {
                    return None;
                }
                rec.backglass_size
                    .map(|(w, h)| (hole::template_rect(w, h), "template"))
            }),
    }?;
    let mut rect = rect_and_source.0;

    let ratio = match policy.aspect {
        Aspect::Hole => None,
        Aspect::Native => policy.native,
        Aspect::Ini => rec.ini_rect.map(|r| r.aspect()),
    };
    if let Some(ratio) = ratio {
        rect = rect.with_aspect(ratio);
        // the content hangs from the top edge of the hole, so a taller box grows
        // downwards; keep it on the image
        if let Some((_, h)) = rec.backglass_size
            && rect.y + rect.h > h as f64
        {
            rect.y = (h as f64 - rect.h).max(0.0);
        }
    }
    Some((rect, rect_and_source.1))
}

/// The ini window sits on the ScoreView (FullDMD) window: normalize it
/// against the ScoreView rectangle on the desktop - the backglass monitor's
/// position plus the ScreenRes.txt DMD area, scaled from the B2S window size
/// to the monitor - and paint that position onto the DMD area inside the
/// backglass image, which is where the DMDImage canvas covers the image.
fn to_scoreview_space(window: Rect, policy: Policy, image: (u32, u32)) -> Option<Rect> {
    let sr = policy.screenres?;
    let (bg_w, bg_h) = sr.backglass?;
    let (ax, ay, aw, ah) = sr.dmd_area?;
    if aw <= 0.0 || ah <= 0.0 || bg_w <= 0.0 || bg_h <= 0.0 {
        return None;
    }
    let mon = policy.backglass;
    let sx = mon.w / bg_w;
    let sy = mon.h / bg_h;
    let scoreview = Rect::new(
        mon.x + (sr.bg_pos.0 + ax) * sx,
        mon.y + (sr.bg_pos.1 + ay) * sy,
        aw * sx,
        ah * sy,
    );
    let nx = (window.x - scoreview.x) / scoreview.w;
    let ny = (window.y - scoreview.y) / scoreview.h;
    let nw = window.w / scoreview.w;
    let nh = window.h / scoreview.h;
    let ix = image.0 as f64 / bg_w;
    let iy = image.1 as f64 / bg_h;
    let canvas = Rect::new(ax * ix, ay * iy, aw * ix, ah * iy);
    Some(Rect::new(
        canvas.x + nx * canvas.w,
        canvas.y + ny * canvas.h,
        nw * canvas.w,
        nh * canvas.h,
    ))
}

/// The ini window in backglass image pixels: the backglass image covers the
/// backglass monitor, so both scales are `image / monitor` and the window's x is
/// read from the monitor's left edge.
///
/// The fit over the authored tags of this installation says the top edge and the
/// width land within a few pixels (dy -21..+20, dw 0..4 %); the height does not,
/// because the ini window has a frame around the DMD content and comes out 16 %
/// to 78 % too tall. That is what `--aspect native` is for.
fn to_image_space(window: Rect, image: (u32, u32), policy: Policy) -> Rect {
    let sx = image.0 as f64 / policy.backglass.w;
    let sy = image.1 as f64 / policy.backglass.h;
    Rect::new(
        (window.x - policy.backglass.x) * sx,
        (window.y - policy.backglass.y) * sy,
        window.w * sx,
        window.h * sy,
    )
}

/// Collects every `*.vpx` in the tree as (directory, file name).
/// One unit of work: a table with its backglass next to it, or a loose
/// backglass without a table.
enum Work {
    Table { dir: PathBuf, name: String },
    Solo { path: PathBuf },
}

impl Work {
    fn cost(&self) -> i64 {
        match self {
            Work::Table { dir, name } => estimate(dir, name),
            Work::Solo { path } => {
                let mb = |bytes: u64| (bytes / 1_048_576).max(1) as i64;
                fs::metadata(path).map(|m| 8 + mb(m.len()) * 3).unwrap_or(8)
            }
        }
    }
}

/// Loose backglasses under `root` that no table claims: a file is skipped
/// when a .vpx of a matching name sits next to it, that pair is scanned as
/// a table. These files have no rom name, so the ini method is out and the
/// stamp rectangle comes from hole detection.
fn collect_solo(root: &Path, limit: Option<usize>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) => {
                let _ = crate::eprintln!("skip {}: {e}", dir.display());
                continue;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let claimed = path.extension().is_some_and(|e| {
                e.eq_ignore_ascii_case("directb2s") || e.eq_ignore_ascii_case("b2spro")
            });
            if !claimed {
                continue;
            }
            let Some(stem) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
                continue;
            };
            if find_vpx(&dir, &stem).is_some() {
                continue;
            }
            out.push(path);
            if let Some(n) = limit
                && out.len() >= n
            {
                return out;
            }
        }
    }
    out
}

fn collect(root: &Path, limit: Option<usize>) -> Result<Vec<(PathBuf, String)>, String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) => {
                let _ = crate::eprintln!("skip {}: {e}", dir.display());
                continue;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_some_and(|e| e == "vpx")
                && let Some(name) = path.file_name().map(|n| n.to_string_lossy().to_string())
            {
                out.push((dir.clone(), name));
                if let Some(n) = limit
                    && out.len() >= n
                {
                    return Ok(out);
                }
            }
        }
    }
    Ok(out)
}

/// Finds the backglass XML for a table: the exact stem first, then the
/// "same up to the (Company Year)" key, and within that the shortest name,
/// which is the main file of a version family.
fn find_b2s(dir: &Path, stem: &str) -> Option<(PathBuf, MatchKind)> {
    let mut exact: Vec<PathBuf> = Vec::new();
    let mut similar: Vec<(String, PathBuf)> = Vec::new();
    let mut ambiguous = false;

    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if !EXTENSIONS.contains(&ext.as_str()) {
            continue;
        }
        let name = path.file_stem()?.to_string_lossy().to_string();
        if stem_eq(stem, &name) {
            exact.push(path.clone());
        } else if match_key(stem) == match_key(&name) {
            similar.push((name, path.clone()));
        }
    }

    if exact.len() == 1 {
        return Some((exact.remove(0), MatchKind::Exact));
    } else if exact.len() > 1 {
        ambiguous = true;
    }

    if similar.len() == 1 {
        return Some((similar.remove(0).1, MatchKind::Similar));
    } else if similar.len() > 1 {
        // sort by name length: the base name is the shortest one; ties go
        // alphabetically, so a version family resolves predictably
        similar.sort_by(|a, b| a.0.cmp(&b.0));
        return Some((similar.remove(0).1, MatchKind::Ambiguous));
    }

    if ambiguous {
        exact.sort();
        return Some((exact.remove(0), MatchKind::Ambiguous));
    }
    None
}

/// The table for a backglass file: the exact stem first, then the same
/// "up to the (Company Year)" key as [`find_b2s`], shortest name wins. This
/// is the reverse direction of `b2s stampdmd <a .directb2s/.b2spro>`.
fn find_vpx(dir: &Path, stem: &str) -> Option<PathBuf> {
    let mut exact: Vec<PathBuf> = Vec::new();
    let mut similar: Vec<(String, PathBuf)> = Vec::new();
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file()
            || !path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("vpx"))
        {
            continue;
        }
        let name = path.file_stem()?.to_string_lossy().to_string();
        if stem_eq(stem, &name) {
            exact.push(path.clone());
        } else if match_key(stem) == match_key(&name) {
            similar.push((name, path.clone()));
        }
    }
    if exact.len() == 1 {
        return Some(exact.remove(0));
    }
    if exact.len() > 1 {
        exact.sort();
        return Some(exact.remove(0));
    }
    if !similar.is_empty() {
        similar.sort_by(|a, b| a.0.cmp(&b.0));
        return Some(similar.remove(0).1);
    }
    None
}

static YEAR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(.*?)\(([^()]*?)(\d{4})[^()]*\)").unwrap());

/// The part of a table name up to and including the `(Company Year)` string,
/// lower-case, spaces squeezed, separators normalised.
fn match_key(name: &str) -> String {
    let Some(caps) = YEAR_RE.captures(name) else {
        return normalize(name);
    };
    let head = caps.get(1).map(|m| m.as_str()).unwrap_or("");
    let company = caps.get(2).map(|m| m.as_str()).unwrap_or("");
    let year = caps.get(3).map(|m| m.as_str()).unwrap_or("");
    format!("{}({}{})", normalize(head), normalize(company), year)
}

fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .collect::<String>()
        .to_lowercase()
}

fn stem_eq(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn year_key_ignores_the_version_tail() {
        let a = "Metallica Pro (Stern 2013) LE 1 0";
        let b = "Metallica Pro (Stern 2013)";
        assert_eq!(match_key(a), match_key(b));
    }

    #[test]
    fn year_key_needs_a_four_digit_year() {
        assert_eq!(match_key("Foo (Bar)"), normalize("Foo (Bar)"));
    }

    #[test]
    fn year_key_keeps_the_company_so_distinct_tables_stay_apart() {
        // the same head with a different publisher is a different table, so the
        // company stays in the key and such a pair falls back to no match
        assert_ne!(
            match_key("Twister (Mylstar 1985)"),
            match_key("Twister (DE 1985)")
        );
        assert_eq!(
            match_key("Twister (Mylstar 1985)"),
            match_key("Twister (Mylstar 1985) v2")
        );
    }

    #[test]
    fn grid_accepts_the_common_shapes() {
        assert_eq!("128x32".parse::<Grid>().unwrap().aspect(), 4.0);
        assert_eq!("192:64".parse::<Grid>().unwrap().aspect(), 3.0);
        assert!("4x0".parse::<Grid>().is_err());
    }

    #[test]
    fn a_scoreview_based_ini_window_maps_through_the_dmd_area() {
        // the monitor is twice the B2S window size, so the ScoreView window
        // on the desktop is the DMD area times two
        let policy = Policy {
            backglass: Rect::new(4000.0, 0.0, 4000.0, 4000.0),
            ini_target: IniTarget::ScoreView,
            screenres: Some(crate::b2s::wizard::ScreenResInfo {
                playfield_w: 2000.0,
                playfield_h: 2000.0,
                backglass: Some((2000.0, 2000.0)),
                dmd_area: Some((100.0, 200.0, 800.0, 600.0)),
                bg_pos: (0.0, 0.0),
            }),
            ..test_policy()
        };
        // the ScoreView window on the desktop: 4000 + 100*2, 0 + 200*2,
        // 800*2, 600*2 = 4200,400,1600,1200. the ini window sits in its
        // top left quadrant: 4200,400,800,600 -> normalized 0,0,0.5,0.5
        let window = Rect::new(4200.0, 400.0, 800.0, 600.0);
        // the DMD area inside the 1000x1000 image: 100*0.5, 200*0.5,
        // 800*0.5, 600*0.5 = 50,100,400,300 -> the quadrant of that: 50,100,200,150
        let got = to_scoreview_space(window, policy, (1000, 1000)).unwrap();
        assert_eq!(
            (got.x as i32, got.y as i32, got.w as i32, got.h as i32),
            (50, 100, 200, 150)
        );
    }

    #[test]
    fn the_scoreview_stamp_is_measured_on_the_dmdimage_canvas() {
        // a hole traced inside the DMDImage is already in the canvas space
        let rec = Record {
            has_dmd_image: true,
            dmd_image_size: Some((1280, 960)),
            backglass_size: Some((2500, 2290)),
            chosen: Some(Rect::new(600.0, 1000.0, 1200.0, 300.0)),
            scoreview_rect: Some(Rect::new(100.0, 200.0, 1200.0, 300.0)),
            ..Default::default()
        };
        let t = stamp_targets(&rec);
        let sv = t.iter().find(|(n, _)| n == "ScoreView").unwrap().1;
        assert_eq!(sv, Rect::new(100.0, 200.0, 1200.0, 300.0));
    }

    #[test]
    fn a_backglass_spot_maps_to_the_dmdimage_over_the_whole_backglass() {
        // no DMD area: the DMDImage covers the backglass, the mapping is
        // the plain ratio - half the backglass is half the canvas
        let rec = Record {
            has_dmd_image: true,
            dmd_image_size: Some((1250, 1145)),
            backglass_size: Some((2500, 2290)),
            chosen: Some(Rect::new(600.0, 1000.0, 1200.0, 300.0)),
            hole_dmd_image: None,
            ..Default::default()
        };
        let sv = to_dmdimage_space(&rec, test_policy()).unwrap();
        assert_eq!(sv, Rect::new(300.0, 500.0, 600.0, 150.0));
    }

    #[test]
    fn a_backglass_spot_maps_to_the_dmdimage_over_the_dmd_area() {
        // ScreenRes: the ScoreView window is the DMD area 1280x960 at
        // 640,480 on a 2560x2880 backglass, painted into a 2500x2290
        // image: the canvas spot, scaled by the 1280x960 DMDImage
        let rec = Record {
            has_dmd_image: true,
            dmd_image_size: Some((1280, 960)),
            backglass_size: Some((2500, 2290)),
            chosen: Some(Rect::new(937.5, 763.5, 625.0, 382.0)),
            hole_dmd_image: None,
            ..Default::default()
        };
        let policy = Policy {
            screenres: Some(crate::b2s::wizard::ScreenResInfo {
                playfield_w: 2560.0,
                playfield_h: 1440.0,
                backglass: Some((2560.0, 2880.0)),
                dmd_area: Some((640.0, 480.0, 1280.0, 960.0)),
                bg_pos: (0.0, 0.0),
            }),
            ..test_policy()
        };
        // the canvas of the DMD area inside the image: 625,381.7 1250x763.3;
        // the chosen spot is at its quarter width and half height, and the
        // 1280x960 DMDImage scales that to 320,480 640x480
        let sv = to_dmdimage_space(&rec, policy).unwrap();
        assert!((sv.x - 320.0).abs() < 0.01, "{sv:?}");
        assert!((sv.y - 480.2).abs() < 0.05, "{sv:?}");
        assert!((sv.w - 640.0).abs() < 0.01, "{sv:?}");
        assert!((sv.h - 480.42).abs() < 0.01, "{sv:?}");
    }

    fn test_policy() -> Policy {
        Policy {
            method: Method::Hole,
            aspect: Aspect::Hole,
            native: None,
            backglass: Rect::new(2560.0, 0.0, 2560.0, 2880.0),
            want_images: true,
            ini_target: IniTarget::Backglass,
            screenres: None,
        }
    }

    #[test]
    fn rect_parses_a_monitor() {
        assert_eq!(
            "2560,0,2560,2880".parse::<Rect>().unwrap(),
            Rect::new(2560.0, 0.0, 2560.0, 2880.0)
        );
        assert!("2560,0,2560".parse::<Rect>().is_err());
        assert!("0,0,0,0".parse::<Rect>().is_err());
    }

    #[test]
    fn the_image_covers_the_backglass_monitor() {
        let policy = Policy {
            method: Method::Ini,
            aspect: Aspect::Hole,
            native: None,
            backglass: "2560,0,2560,2880".parse().unwrap(),
            want_images: false,
            ini_target: Default::default(),
            screenres: None,
        };
        // afm_113b: a window at the left edge of the backglass monitor lands at
        // the left edge of a 2000 px image, scaled by 2000/2560
        let window = Rect::new(2560.0, 0.0, 1280.0, 2880.0);
        assert_eq!(
            to_image_space(window, (2000, 1565), policy),
            Rect::new(0.0, 0.0, 1000.0, 1565.0)
        );
    }

    #[test]
    fn native_aspect_keeps_the_width_and_hangs_from_the_top() {
        let policy = Policy {
            method: Method::Auto,
            aspect: Aspect::Native,
            native: Some(4.0),
            backglass: "2560,0,2560,2880".parse().unwrap(),
            want_images: false,
            ini_target: Default::default(),
            screenres: None,
        };
        let rec = Record {
            hole_backglass: Some(Rect::new(504.0, 1287.0, 949.0, 164.0)),
            backglass_size: Some((2000, 1565)),
            grill_height: Some(300.0),
            dmd_image_size: Some((1920, 480)),
            has_dmd_image: true,
            ..Default::default()
        };
        assert_eq!(
            pick(policy, &rec),
            Some((Rect::new(504.0, 1287.0, 949.0, 237.25), "hole"))
        );
    }

    #[test]
    fn native_aspect_stays_on_the_image() {
        let policy = Policy {
            method: Method::Auto,
            aspect: Aspect::Native,
            native: Some(2.0),
            backglass: "2560,0,2560,2880".parse().unwrap(),
            want_images: false,
            ini_target: Default::default(),
            screenres: None,
        };
        let rec = Record {
            hole_backglass: Some(Rect::new(10.0, 900.0, 1000.0, 100.0)),
            backglass_size: Some((2000, 1000)),
            grill_height: Some(300.0),
            ini_section: true,
            ..Default::default()
        };
        // 1000/2 = 500 tall, y = 900 would run to 1400, so the box is pushed up
        assert_eq!(
            pick(policy, &rec),
            Some((Rect::new(10.0, 500.0, 1000.0, 500.0), "hole"))
        );
    }

    #[test]
    fn a_grill_strip_hole_is_never_chosen() {
        let policy = Policy {
            method: Method::Auto,
            aspect: Aspect::Hole,
            native: None,
            backglass: "2560,0,2560,2880".parse().unwrap(),
            want_images: true,
            ini_target: Default::default(),
            screenres: None,
        };
        let rec = Record {
            scoreview_rect: None,
            hole_dmd_image: Some(Rect::new(111.0, 618.0, 1696.0, 419.0)),
            grill_height: Some(400.0),
            ..Default::default()
        };
        assert_eq!(pick(policy, &rec), None);
    }

    #[test]
    fn the_template_method_falls_back_to_the_authored_position() {
        let policy = Policy {
            method: Method::Template,
            aspect: Aspect::Hole,
            native: None,
            backglass: "2560,0,2560,2880".parse().unwrap(),
            want_images: true,
            ini_target: Default::default(),
            screenres: None,
        };
        let rec = Record {
            grill_height: Some(180.0),
            backglass_size: Some((2000, 1565)),
            dmd_image_size: Some((1920, 480)),
            has_dmd_image: true,
            hole_scanned: true,
            ..Default::default()
        };
        assert_eq!(
            pick(policy, &rec),
            Some((hole::template_rect(2000, 1565), "template"))
        );
    }

    #[test]
    fn a_rejected_flat_rectangle_blocks_the_template() {
        let policy = Policy {
            method: Method::Template,
            aspect: Aspect::Hole,
            native: None,
            backglass: "2560,0,2560,2880".parse().unwrap(),
            want_images: true,
            ini_target: Default::default(),
            screenres: None,
        };
        // Raven (Gottlieb 1986): a 471x76 stripe in the grill band, 6.2:1. The
        // tracer saw it, the DMD-shape filter rejected it. The file has a grill
        // height, but that stripe is not a window, so the template must not
        // invent one.
        let rec = Record {
            grill_height: Some(304.0),
            backglass_size: Some((1280, 1024)),
            dmd_image_size: Some((696, 232)),
            has_dmd_image: true,
            hole_scanned: true,
            hole_seen: 1,
            ..Default::default()
        };
        assert_eq!(pick(policy, &rec), None);
    }

    #[test]
    fn a_backglass_that_was_not_inspected_gets_no_template() {
        let policy = Policy {
            method: Method::Template,
            aspect: Aspect::Hole,
            native: None,
            backglass: "2560,0,2560,2880".parse().unwrap(),
            want_images: false,
            ini_target: Default::default(),
            screenres: None,
        };
        let rec = Record {
            grill_height: Some(304.0),
            backglass_size: Some((1280, 1024)),
            dmd_image_size: Some((696, 232)),
            has_dmd_image: true,
            ..Default::default()
        };
        assert_eq!(pick(policy, &rec), None);
    }

    #[test]
    fn a_backglass_without_a_grill_gets_no_template() {
        let policy = Policy {
            method: Method::Template,
            aspect: Aspect::Hole,
            native: None,
            backglass: "2560,0,2560,2880".parse().unwrap(),
            want_images: true,
            ini_target: Default::default(),
            screenres: None,
        };
        // no grill, no hole, no ini, no authored tag: nothing says a DMD
        // window exists, so the file must stay untouched
        let rec = Record {
            grill_height: Some(0.0),
            backglass_size: Some((2000, 1565)),
            ..Default::default()
        };
        assert_eq!(pick(policy, &rec), None);
        let rec = Record {
            backglass_size: Some((2000, 1565)),
            ..Default::default()
        };
        assert_eq!(pick(policy, &rec), None);
    }

    #[test]
    fn the_template_method_keeps_an_authored_tag() {
        let policy = Policy {
            method: Method::Template,
            aspect: Aspect::Hole,
            native: None,
            backglass: "2560,0,2560,2880".parse().unwrap(),
            want_images: true,
            ini_target: Default::default(),
            screenres: None,
        };
        let authored = Rect::new(500.0, 1240.0, 1024.0, 216.0);
        let rec = Record {
            existing_dmd: Some(authored),
            backglass_size: Some((2000, 1565)),
            grill_height: Some(300.0),
            ..Default::default()
        };
        assert_eq!(pick(policy, &rec), Some((authored, "tag")));
        // a placeholder tag of zero size is not evidence
        let rec = Record {
            existing_dmd: Some(Rect::default()),
            backglass_size: Some((2000, 1565)),
            grill_height: Some(300.0),
            ..Default::default()
        };
        assert_eq!(pick(policy, &rec), None);
    }

    #[test]
    fn a_grill_height_of_zero_blocks_even_an_authored_tag() {
        // GrillHeight 0 is the prerequisite for every stamp, not just the
        // template: a full-image backglass has no DMD cut-out to position.
        let policy = Policy {
            method: Method::Template,
            aspect: Aspect::Hole,
            native: None,
            backglass: "2560,0,2560,2880".parse().unwrap(),
            want_images: true,
            ini_target: Default::default(),
            screenres: None,
        };
        let rec = Record {
            existing_dmd: Some(Rect::new(500.0, 1240.0, 1024.0, 216.0)),
            ini_in_image: Some(Rect::new(480.0, 1255.0, 1033.0, 209.0)),
            backglass_size: Some((2000, 1565)),
            grill_height: Some(0.0),
            ..Default::default()
        };
        assert_eq!(pick(policy, &rec), None);
    }

    #[test]
    fn an_authored_tag_with_the_wrong_shape_is_not_evidence() {
        let policy = Policy {
            method: Method::Template,
            aspect: Aspect::Hole,
            native: None,
            backglass: "2560,0,2560,2880".parse().unwrap(),
            want_images: true,
            ini_target: Default::default(),
            screenres: None,
        };
        // Elvira and the Party Monsters: the 8.9:1 stripe that an older run
        // wrote is not a DMD window, even though it is now a tag in the file.
        let rec = Record {
            existing_dmd: Some(Rect::new(179.0, 1890.0, 934.0, 105.0)),
            backglass_size: Some((2200, 2200)),
            grill_height: Some(306.0),
            dmd_image_size: Some((1920, 480)),
            has_dmd_image: true,
            hole_scanned: true,
            hole_seen: 1,
            ..Default::default()
        };
        assert_eq!(pick(policy, &rec), None);
    }

    #[test]
    fn the_ini_loses_to_the_hole_and_the_template_is_last() {
        let policy = Policy {
            method: Method::Auto,
            aspect: Aspect::Hole,
            native: None,
            backglass: "2560,0,2560,2880".parse().unwrap(),
            want_images: true,
            ini_target: Default::default(),
            screenres: None,
        };
        let rec = Record {
            hole_backglass: Some(Rect::new(500.0, 1240.0, 1024.0, 216.0)),
            ini_in_image: Some(Rect::new(480.0, 1255.0, 1033.0, 209.0)),
            backglass_size: Some((2000, 1565)),
            grill_height: Some(300.0),
            ini_section: true,
            ..Default::default()
        };
        // the two agree on the center: the tuned ini window wins
        assert_eq!(
            pick(policy, &rec),
            Some((Rect::new(480.0, 1255.0, 1033.0, 209.0), "ini"))
        );
        // a window from another machine, a quarter of the strip away: the hole
        let rec = Record {
            hole_backglass: Some(Rect::new(500.0, 1240.0, 1024.0, 216.0)),
            ini_in_image: Some(Rect::new(1800.0, 1255.0, 300.0, 130.0)),
            backglass_size: Some((2000, 1565)),
            grill_height: Some(300.0),
            ini_section: true,
            ..Default::default()
        };
        assert_eq!(
            pick(policy, &rec),
            Some((Rect::new(500.0, 1240.0, 1024.0, 216.0), "hole"))
        );
        // without a hole, auto uses the ini and does not reach the template
        let rec = Record {
            ini_in_image: Some(Rect::new(480.0, 1255.0, 1033.0, 209.0)),
            backglass_size: Some((2000, 1565)),
            grill_height: Some(300.0),
            ini_section: true,
            ..Default::default()
        };
        assert_eq!(
            pick(policy, &rec),
            Some((Rect::new(480.0, 1255.0, 1033.0, 209.0), "ini"))
        );
    }

    #[test]
    fn a_hole_without_dmd_evidence_is_not_stamped() {
        let policy = Policy {
            method: Method::Hole,
            aspect: Aspect::Hole,
            native: None,
            backglass: "2560,0,2560,2880".parse().unwrap(),
            want_images: true,
            ini_target: Default::default(),
            screenres: None,
        };
        // Heavy Metal (Rowamet 1983): a plausible 4.5:1 flat rectangle in the
        // art, but the backglass has no DMDImage, no tag and no ini section.
        // The game has no DMD; the rectangle is a score panel.
        let hole = Rect::new(425.0, 798.0, 1077.0, 241.0);
        let rec = Record {
            hole_backglass: Some(hole),
            backglass_size: Some((2400, 1080)),
            grill_height: Some(320.0),
            ..Default::default()
        };
        assert_eq!(pick(policy, &rec), None);

        // once the backglass carries a DMD image, the same hole is evidence
        let rec = Record {
            dmd_image_size: Some((1920, 480)),
            has_dmd_image: true,
            ..rec
        };
        assert_eq!(pick(policy, &rec), Some((hole, "hole")));
    }

    #[test]
    fn an_ini_section_unlocks_the_template() {
        let policy = Policy {
            method: Method::Template,
            aspect: Aspect::Hole,
            native: None,
            backglass: "2560,0,2560,2880".parse().unwrap(),
            want_images: true,
            ini_target: Default::default(),
            screenres: None,
        };
        let rec = Record {
            grill_height: Some(300.0),
            backglass_size: Some((2000, 1565)),
            hole_scanned: true,
            ini_section: true,
            ..Default::default()
        };
        assert_eq!(
            pick(policy, &rec),
            Some((hole::template_rect(2000, 1565), "template"))
        );
    }

    #[test]
    fn a_tiny_grill_blocks_every_method() {
        // Triple Strike / Joust: GrillHeight 1-2 passes a literal `> 0`, but it
        // is not a grill strip.
        let hole = Rect::new(500.0, 1240.0, 1024.0, 216.0);
        for method in [Method::Hole, Method::Ini, Method::Auto, Method::Template] {
            let policy = Policy {
                method,
                aspect: Aspect::Hole,
                native: None,
                backglass: "2560,0,2560,2880".parse().unwrap(),
                want_images: true,
                ini_target: Default::default(),
                screenres: None,
            };
            let rec = Record {
                hole_backglass: Some(hole),
                ini_in_image: Some(Rect::new(480.0, 1255.0, 1033.0, 209.0)),
                existing_dmd: Some(hole),
                dmd_image_size: Some((1920, 480)),
                ini_section: true,
                backglass_size: Some((2000, 1565)),
                grill_height: Some(2.0),
                has_dmd_image: false,
                hole_scanned: true,
                ..Default::default()
            };
            assert_eq!(pick(policy, &rec), None, "{method:?} accepted a 2 px grill");
        }
    }

    #[test]
    fn the_grill_needs_ten_pixels_and_one_percent_of_the_height() {
        let policy = Policy {
            method: Method::Hole,
            aspect: Aspect::Hole,
            native: None,
            backglass: "2560,0,2560,2880".parse().unwrap(),
            want_images: true,
            ini_target: Default::default(),
            screenres: None,
        };
        let hole = Rect::new(300.0, 700.0, 600.0, 160.0);
        // Bone Busters Inc.: 10 px on a 944 px image is just over 1 %
        let rec = Record {
            hole_backglass: Some(hole),
            dmd_image_size: Some((1920, 480)),
            backglass_size: Some((1259, 944)),
            grill_height: Some(10.0),
            has_dmd_image: false,
            ..Default::default()
        };
        assert_eq!(pick(policy, &rec), Some((hole, "hole")));

        // 12 px, but only 0.47 % of a 2560 px backglass: the strip is too thin
        // to be the panel a DMD window is cut into
        let rec = Record {
            backglass_size: Some((2000, 2560)),
            grill_height: Some(12.0),
            ..rec
        };
        assert_eq!(pick(policy, &rec), None);
    }
}

#[cfg(test)]
mod auto_agreement_tests {
    use super::*;

    fn policy() -> Policy {
        Policy {
            method: Method::Auto,
            aspect: Aspect::Hole,
            native: None,
            backglass: "3840,0,2560,2880".parse().unwrap(),
            want_images: true,
            ini_target: Default::default(),
            screenres: None,
        }
    }

    #[test]
    fn a_matching_ini_window_wins_over_the_hole() {
        // afm on the cabinet: the traced hole is 40 px short, the owner's ini
        // window sits right on the window - the centers agree
        let rec = Record {
            hole_backglass: Some(Rect::new(498.0, 1285.0, 970.0, 163.0)),
            ini_in_image: Some(Rect::new(482.0, 1250.0, 1023.0, 210.0)),
            backglass_size: Some((2500, 3022)),
            grill_height: Some(373.0),
            ini_section: true,
            ..Default::default()
        };
        assert_eq!(
            pick(policy(), &rec),
            Some((Rect::new(482.0, 1250.0, 1023.0, 210.0), "ini"))
        );
    }

    #[test]
    fn a_foreign_ini_window_loses_to_the_hole() {
        // a default window from another machine: the center is a fifth of the
        // image away, the hole is the one that sits on the painted window
        let rec = Record {
            hole_backglass: Some(Rect::new(300.0, 1285.0, 970.0, 163.0)),
            ini_in_image: Some(Rect::new(2200.0, 1250.0, 300.0, 163.0)),
            backglass_size: Some((2500, 3022)),
            grill_height: Some(373.0),
            ini_section: true,
            ..Default::default()
        };
        assert_eq!(
            pick(policy(), &rec),
            Some((Rect::new(300.0, 1285.0, 970.0, 163.0), "hole"))
        );
    }
}
