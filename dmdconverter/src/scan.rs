//! Collects the tables of a folder tree, matches them to their B2S backglass
//! file, and works out the DMD rectangle to stamp.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};

use regex::Regex;
use serde::Serialize;

use crate::b2s::{self, B2sFile};
use crate::dmdini::DmdDeviceIni;
use crate::hole;
use crate::rom;
use crate::stamp;
use crate::{default_ini, Aspect, Grid, Method, Rect};

/// The extensions that hold the backglass XML. The real world is messy: `.B2SPro`
/// and `.directb2sPRO` turn up in the same folder as `.directb2s`, and
/// `.directb2st` / `.directb2sss` / `.directb2s_no` look like typos, leftovers
/// of split packs, or backup copies. Only the two documented extensions are
/// candidates; a table that has nothing but a `.directb2st` is reported as
/// missing, because writing the wrong file is worse than doing nothing.
const EXTENSIONS: &[&str] = &["directb2s", "b2spro"];

#[derive(clap::Args, Debug, Clone)]
pub struct ScanArgs {
    /// folder tree to scan for *.vpx
    pub root: PathBuf,

    /// path of DmdDevice.ini, defaults to the standard VPinball location
    #[arg(long)]
    pub ini: Option<PathBuf>,

    /// how to derive the rectangle
    #[arg(long, value_enum, default_value = "auto")]
    pub method: Method,

    /// skip hole detection: no image decoding, so ini-only and much faster
    #[arg(long)]
    pub no_hole: bool,

    /// write the collected data as JSON for analysis
    #[arg(long)]
    pub json: Option<PathBuf>,

    /// write the DMD element into the .directb2s/.b2spro file (in place)
    #[arg(long)]
    pub write: bool,

    /// do not keep a .bak copy when writing
    #[arg(long)]
    pub no_backup: bool,

    /// GlassRoughness value to write
    #[arg(long, default_value_t = 0.1)]
    pub glass_roughness: f64,

    /// aspect ratio of the stamped rectangle: keep the hole, keep the ini
    /// window's ratio, or derive the height from `--native-grid`. Every authored
    /// tag on this installation is 4:1 (one 4.74), so `--native-grid 4x1`
    /// reproduces the authoring convention
    #[arg(long, value_enum, default_value = "hole")]
    pub aspect: Aspect,

    /// the rom's dot grid, needed by `--aspect native`: `128x32` for the WPC
    /// era, `192x64` for SAM, and `4x1` for the plain 4:1 content box the
    /// authoring tools draw. Resolving it per rom needs the rom database, which
    /// this tool does not depend on, so it stays an option.
    #[arg(long)]
    pub native_grid: Option<Grid>,

    /// the backglass monitor in desktop pixels, `x,y,w,h`: the ini window is in
    /// desktop pixels, the `<DMD>` tag is in backglass image pixels, and the
    /// image covers this monitor
    #[arg(long, default_value = "2560,0,2560,2880")]
    pub backglass_monitor: Rect,

    /// stop after N tables
    #[arg(long)]
    pub limit: Option<usize>,

    /// how many tables to read at a time. The work per table is decoding the
    /// embedded images and parsing 20-50 MB of XML, so more than one core pays
    #[arg(long, default_value_t = default_jobs())]
    pub jobs: usize,
}

fn default_jobs() -> usize {
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
    pub ini_rect: Option<Rect>,
    pub existing_dmd: Option<Rect>,
    pub grill_height: Option<f64>,
    pub backglass_size: Option<(u32, u32)>,
    pub dmd_image_size: Option<(u32, u32)>,
    pub hole_backglass: Option<Rect>,
    pub hole_dmd_image: Option<Rect>,
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

pub fn scan(args: ScanArgs) -> Result<Vec<Record>, String> {
    let ini_path = args.ini.clone().unwrap_or_else(default_ini);
    let ini = match DmdDeviceIni::load(&ini_path) {
        Ok(ini) => {
            eprintln!(
                "DmdDevice.ini: {} ({} sections)",
                ini_path.display(),
                ini.section_count()
            );
            Some(ini)
        }
        Err(e) => {
            eprintln!("warning: {e}, ini lookups disabled");
            None
        }
    };

    let native = match args.aspect {
        Aspect::Native => match args.native_grid {
            Some(grid) => Some(grid.aspect()),
            None => {
                return Err(
                    "--aspect native needs --native-grid, e.g. `--native-grid 128x32`".into(),
                )
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
    };

    let tables = collect(&args.root, args.limit)?;
    eprintln!(
        "found {} tables under {}",
        tables.len(),
        args.root.display()
    );

    let jobs = args.jobs.max(1);
    let started = std::time::Instant::now();
    let total = tables.len();
    // The work per table is decoding the embedded images and parsing 20-50 MB of
    // XML, and the peak of that is the XML of one file, so the workers share a
    // budget of in-flight megabytes: it is what keeps 12 cores from asking for
    // 100 GB at once.
    let budget = Budget::default();
    let mut records = if jobs == 1 || total < 2 {
        let mut out = Vec::with_capacity(total);
        for (dir, vpx_name) in &tables {
            let cost = estimate(dir, vpx_name);
            budget.claim(cost);
            out.push(scan_catch(dir, vpx_name, &ini, policy));
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
                let tables = &tables;
                let ini = &ini;
                scope.spawn(move || loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    let Some((dir, vpx_name)) = tables.get(i) else {
                        break;
                    };
                    let cost = estimate(dir, vpx_name);
                    while !budget.claim(cost) {
                        // the budget is held by bigger files, wait for one
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                    let rec = scan_catch(dir, vpx_name, ini, policy);
                    budget.release(cost);
                    out.lock().unwrap().push(rec);
                    report_progress(done.fetch_add(1, Ordering::SeqCst) + 1, total);
                });
            }
        });
        out.into_inner()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    };
    eprintln!(
        "{} tables in {:.0}s, peak {} MB in flight",
        records.len(),
        started.elapsed().as_secs_f64(),
        budget.peak()
    );
    records.sort_by(|a, b| a.vpx.cmp(&b.vpx));

    crate::report::print(&records);
    if let Some(path) = &args.json {
        let json = serde_json::to_string_pretty(&records).map_err(|e| e.to_string())?;
        fs::write(path, json).map_err(|e| format!("write {}: {e}", path.display()))?;
        eprintln!("json written to {}", path.display());
    }
    // the stamp pass: without --write it only reports the planned edits
    let stats = stamp::stamp(&records, args.glass_roughness, args.write, !args.no_backup);
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
    if let Some((path, _)) = find_b2s(dir, &stem.to_string_lossy()) {
        if let Ok(len) = fs::metadata(&path).map(|m| m.len()) {
            // bytes + String + the decoded images of one payload
            out += mb(len) * 3;
        }
    }
    if let Ok(len) = fs::metadata(dir.join(vpx_name)).map(|m| m.len()) {
        out += mb(len) / 2;
    }
    out
}

fn report_progress(done: usize, total: usize) {
    if done.is_multiple_of(100) || done == total {
        eprintln!("  {done}/{total} tables");
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

    let file: B2sFile = match b2s::probe(&b2s_path, policy.want_images) {
        Ok(f) => f,
        Err(e) => {
            rec.notes.push(format!("b2s read failed: {e}"));
            return rec;
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

    // ROM name: sidecar .vbs first, then the script embedded in the .vpx
    match rom::game_name_from_vpx(&vpx) {
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

    // DmdDevice.ini lookup
    if let (Some(ini), Some(rom)) = (ini, rec.rom.as_deref()) {
        let dmd = ini.lookup(rom);
        rec.ini_section = dmd.section_found;
        rec.ini_rect = dmd.rect();
        if let (Some(window), Some((w, h))) = (rec.ini_rect, rec.backglass_size) {
            rec.ini_in_image = Some(to_image_space(window, (w, h), policy));
        }
        if !dmd.section_found {
            rec.notes
                .push(format!("no [{rom}] section in DmdDevice.ini"));
        }
    }

    // hole detection. Only the backglass image is in the coordinate space of the
    // <DMD> tag; a hole in the grill strip (DMDImage) is recorded for diagnosis
    // and never stamped, because its coordinates belong to a different image.
    if policy.want_images {
        if let Some(bytes) = &file.backglass_png {
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
                            if grill > 0.0 && hole::pick_in_grill(&[found], h, grill).is_none() {
                                rec.notes
                                    .push("backglass hole is outside the grill band".to_string());
                            }
                        }
                    }
                }
                Err(e) => rec.notes.push(format!("backglass hole: {e}")),
            }
        }
        if let Some(bytes) = &file.dmd_image_png {
            match hole::find_holes_in_bytes(bytes, 0.5) {
                Ok((cands, _)) => rec.hole_dmd_image = cands.first().copied(),
                Err(e) => rec.notes.push(format!("dmd image hole: {e}")),
            }
        }
    }

    if let Some((rect, source)) = pick(policy, &rec) {
        rec.chosen = Some(rect);
        rec.source = Some(source.to_string());
    } else if !has_grill(&rec) {
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
    } else if !has_dmd_evidence(&rec) {
        if rec.hole_backglass.is_some() {
            rec.notes.push(
                "flat rectangle found but no DMD evidence (no DMDImage, tag or ini section): not stamping, left untouched"
                    .into(),
            );
        } else {
            rec.notes.push(
                "no DMD evidence (no DMDImage, tag or ini section): nothing to stamp, left untouched"
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
    rec
}

/// A grill strip has to be a real strip. 1-5 px values do occur in the wild
/// (one installation had 1, 2 and 3 px) and pass a literal `GrillHeight > 0`,
/// but no speaker panel is one pixel high: they are leftover or default values.
/// The smallest real grill of that installation is 10 px on a 944 px image,
/// about 1 % of the height, which is the second bound.
pub const MIN_GRILL_PX: f64 = 10.0;
pub const MIN_GRILL_FRAC: f64 = 0.01;

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
/// authored tag is the author's word, an ini section is the user's. Without one
/// of these, a detected rectangle is a flat band of the artwork, not a window:
/// the 8 false stamps of one library had no tag, and most had no DMDImage.
fn has_dmd_evidence(rec: &Record) -> bool {
    rec.ini_section || rec.dmd_image_size.is_some() || authored_tag(rec).is_some()
}

/// The rectangle to stamp and where it came from.
fn pick(policy: Policy, rec: &Record) -> Option<(Rect, &'static str)> {
    // A real grill strip is the prerequisite for every stamp: a backglass
    // without one has no DMD cut-out, and the tracer finds flat bands of the
    // artwork instead.
    if !has_grill(rec) {
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
        // the ini window is on the desktop, the hole is already where the tag
        // wants its numbers, so the hole wins
        Method::Auto => from_hole
            .map(|r| (r, "hole"))
            .or_else(|| from_ini.map(|r| (r, "ini"))),
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
        if let Some((_, h)) = rec.backglass_size {
            if rect.y + rect.h > h as f64 {
                rect.y = (h as f64 - rect.h).max(0.0);
            }
        }
    }
    Some((rect, rect_and_source.1))
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
fn collect(root: &Path, limit: Option<usize>) -> Result<Vec<(PathBuf, String)>, String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("skip {}: {e}", dir.display());
                continue;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_some_and(|e| e == "vpx") {
                if let Some(name) = path.file_name().map(|n| n.to_string_lossy().to_string()) {
                    out.push((dir.clone(), name));
                    if let Some(n) = limit {
                        if out.len() >= n {
                            return Ok(out);
                        }
                    }
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
        };
        let rec = Record {
            hole_backglass: Some(Rect::new(504.0, 1287.0, 949.0, 164.0)),
            backglass_size: Some((2000, 1565)),
            grill_height: Some(300.0),
            dmd_image_size: Some((1920, 480)),
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
        };
        let rec = Record {
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
        };
        let rec = Record {
            grill_height: Some(180.0),
            backglass_size: Some((2000, 1565)),
            dmd_image_size: Some((1920, 480)),
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
        };
        // Raven (Gottlieb 1986): a 471x76 stripe in the grill band, 6.2:1. The
        // tracer saw it, the DMD-shape filter rejected it. The file has a grill
        // height, but that stripe is not a window, so the template must not
        // invent one.
        let rec = Record {
            grill_height: Some(304.0),
            backglass_size: Some((1280, 1024)),
            dmd_image_size: Some((696, 232)),
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
        };
        let rec = Record {
            grill_height: Some(304.0),
            backglass_size: Some((1280, 1024)),
            dmd_image_size: Some((696, 232)),
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
        };
        // Elvira and the Party Monsters: the 8.9:1 stripe that an older run
        // wrote is not a DMD window, even though it is now a tag in the file.
        let rec = Record {
            existing_dmd: Some(Rect::new(179.0, 1890.0, 934.0, 105.0)),
            backglass_size: Some((2200, 2200)),
            grill_height: Some(306.0),
            dmd_image_size: Some((1920, 480)),
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
        };
        let rec = Record {
            hole_backglass: Some(Rect::new(500.0, 1240.0, 1024.0, 216.0)),
            ini_in_image: Some(Rect::new(480.0, 1255.0, 1033.0, 209.0)),
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
            };
            let rec = Record {
                hole_backglass: Some(hole),
                ini_in_image: Some(Rect::new(480.0, 1255.0, 1033.0, 209.0)),
                existing_dmd: Some(hole),
                dmd_image_size: Some((1920, 480)),
                ini_section: true,
                backglass_size: Some((2000, 1565)),
                grill_height: Some(2.0),
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
        };
        let hole = Rect::new(300.0, 700.0, 600.0, 160.0);
        // Bone Busters Inc.: 10 px on a 944 px image is just over 1 %
        let rec = Record {
            hole_backglass: Some(hole),
            dmd_image_size: Some((1920, 480)),
            backglass_size: Some((1259, 944)),
            grill_height: Some(10.0),
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
