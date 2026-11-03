mod b2s;
mod dmdini;
mod hole;
mod report;
mod rom;
mod scan;
mod stamp;

use std::fs;
use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use serde::Serialize;

#[derive(Parser)]
#[command(about = "Stamp the DMD rectangle into B2S backglass XML files")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scan a folder tree for .vpx tables and report the DMD stamp for each
    Scan(scan::ScanArgs),
}

fn main() -> Result<(), String> {
    // a damaged table is a per table note, not a wall of backtrace in the log;
    // scan::scan_catch turns the panic payload into that note
    std::panic::set_hook(Box::new(|info| eprintln!("  note  {info}")));
    let cli = Cli::parse();
    match cli.command {
        Command::Scan(args) => scan::scan(args).map(|_| ()),
    }
}

/// How to derive the DMD rectangle
#[derive(ValueEnum, Clone, Copy, PartialEq, Debug, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Method {
    /// hole detection in the embedded image, no DmdDevice.ini needed
    #[default]
    Hole,
    /// DmdDevice.ini [cGameName] virtualdmd values
    Ini,
    /// hole detection when it worked, ini values otherwise
    Auto,
    /// hole, then ini, then the tag the author already wrote, and the position
    /// the authoring tools use for backglasses with a grill strip. A file with
    /// no grill and no DMD window is left untouched
    Template,
}

impl std::fmt::Display for Method {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Method::Hole => "hole",
            Method::Ini => "ini",
            Method::Auto => "auto",
            Method::Template => "template",
        };
        f.write_str(s)
    }
}

/// Which aspect ratio the stamped rectangle should get.
///
/// The sources disagree by more than rounding, so the ratio has to be chosen
/// rather than derived: the visible glass hole is wider than the DMD content
/// (Attack from Mars: 4.74 for the hole vs the ROM's 4:1), and the ini window
/// is narrower than both (3.44).
#[derive(ValueEnum, Clone, Copy, PartialEq, Debug, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Aspect {
    /// keep the detected hole as is, glass border included: what the authoring
    /// tools produce
    #[default]
    Hole,
    /// the ROM's dot grid, e.g. 128x32 = 4:1: keep the width, derive the
    /// height. Needs `--native-grid`
    Native,
    /// the ratio of the ini window
    Ini,
}

impl std::fmt::Display for Aspect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Aspect::Hole => "hole",
            Aspect::Native => "native",
            Aspect::Ini => "ini",
        };
        f.write_str(s)
    }
}

/// A DMD dot grid, e.g. 128x32 for the WPC era, 192x64 for SAM.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Grid {
    pub cols: u32,
    pub rows: u32,
}

impl Grid {
    pub fn aspect(&self) -> f64 {
        self.cols as f64 / self.rows as f64
    }
}

impl std::str::FromStr for Grid {
    type Err = String;
    /// `128x32`, `128:32` or `128 32`
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let split = s
            .find(['x', 'X', ':', ',', ' '])
            .ok_or("expected `colsxrows`")?;
        let cols = s[..split]
            .trim()
            .parse::<u32>()
            .map_err(|e| e.to_string())?;
        let rows = s[split + 1..]
            .trim()
            .parse::<u32>()
            .map_err(|e| e.to_string())?;
        if cols == 0 || rows == 0 {
            return Err("a grid needs positive columns and rows".into());
        }
        Ok(Grid { cols, rows })
    }
}

impl std::fmt::Display for Grid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}x{}", self.cols, self.rows)
    }
}

/// A rectangle in image pixels
#[derive(Clone, Copy, PartialEq, Debug, Default, Serialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Rect { x, y, w, h }
    }
    pub fn aspect(&self) -> f64 {
        if self.h == 0.0 {
            0.0
        } else {
            self.w / self.h
        }
    }

    /// The same box with a given ratio, anchored at the top left corner: the
    /// DMD content hangs from the top edge of the hole, which is the one
    /// coordinate all three sources agree on within a few pixels.
    pub fn with_aspect(&self, aspect: f64) -> Rect {
        if aspect <= 0.0 {
            return *self;
        }
        Rect {
            h: self.w / aspect,
            ..*self
        }
    }
}

impl std::str::FromStr for Rect {
    type Err = String;
    /// `x,y,width,height`, as desktop pixels for the monitor options
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = s.split([';', ',']).map(str::trim).collect();
        if parts.len() != 4 {
            return Err(format!("expected `x,y,w,h`, got {s}"));
        }
        let n = |i: usize| parts[i].parse::<f64>().map_err(|e| format!("{s}: {e}"));
        let rect = Rect::new(n(0)?, n(1)?, n(2)?, n(3)?);
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return Err(format!("{s}: width and height must be positive"));
        }
        Ok(rect)
    }
}

impl std::fmt::Display for Rect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:.0},{:.0} {:.0}x{:.0}", self.x, self.y, self.w, self.h)
    }
}

/// Candidate locations of the `DmdDevice.ini`. The file in the current folder
/// comes first: the community ini with the per-ROM sections is the useful one,
/// while the installed files are usually the stock template, whose only
/// sections are `[global]`, `[virtualdmd]` and the hardware back ends.
pub const INI_CANDIDATES: &[&str] = &[
    "DmdDevice.ini",
    r"D:\vPinball\VPinMAME\DMDext\DmdDevice.ini",
    r"D:\vPinball\VPinMAME\DmdDevice.ini",
    r"D:\vPinball\Visual Pinball\VPinMAME\DMDext\DmdDevice.ini",
];

/// First candidate that exists and is not empty, else the first candidate.
pub fn default_ini() -> PathBuf {
    INI_CANDIDATES
        .iter()
        .map(PathBuf::from)
        .find(|p| fs::metadata(p).map(|m| m.len() > 0).unwrap_or(false))
        .unwrap_or_else(|| PathBuf::from(INI_CANDIDATES[0]))
}
