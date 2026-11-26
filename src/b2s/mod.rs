//! DirectB2S / B2S-Pro backglass files (`.directb2s` / `.b2spro`).
//!
//! Both formats are the same XML: a `DirectB2SData` root carrying the images
//! as base64 payloads (25-53 MB per file) and, for B2S with a DMD cut-out, the
//! grill strip height. The B2S backglass server reads a `<DMD>` element inside
//! `<Displays>` to know where inside the backglass image the DMD is punched
//! out; the `b2s stampdmd` command writes that element, deriving the
//! rectangle from the image (hole detection), from `DmdDevice.ini`, from the
//! tag the author already wrote, or from the position the authoring tools use.
//!
//! The design notes of the standalone tool this was integrated from live in
//! [`docs/b2s-dmd.md`](../../../docs/b2s-dmd.md).

pub mod dmdini;
pub mod extract;
pub mod hole;
pub mod report;
pub mod rom;
pub mod scan;
pub mod stamp;
pub mod wizard;
pub mod xml;

use std::fmt::Display;
use std::path::PathBuf;
use std::str::FromStr;

use serde::Serialize;

/// A rectangle in image pixels.
/// Serialized as one `"x,y,w,h"` string: a record's rects all live in the
/// same coordinate space (backglass image pixels), so the eye can subtract
/// them. `x,y,w,h` is what the DMD tag itself uses.
impl Serialize for Rect {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format!(
            "{:.0},{:.0},{:.0},{:.0}",
            self.x, self.y, self.w, self.h
        ))
    }
}

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    /// `"x,y,w,h"`, the format of the DMD tag attributes, rounded to pixels
    pub fn to_csv(&self) -> String {
        format!("{:.0},{:.0},{:.0},{:.0}", self.x, self.y, self.w, self.h)
    }

    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Rect { x, y, w, h }
    }
    pub fn aspect(&self) -> f64 {
        if self.h == 0.0 { 0.0 } else { self.w / self.h }
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

impl Display for Rect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:.0},{:.0} {:.0}x{:.0}", self.x, self.y, self.w, self.h)
    }
}

/// Which window the DMD rectangles of `DmdDevice.ini` describe. The classic
/// cabinets cut the DMD out of the backglass, and the ini windows sit on the
/// backglass monitor above that cut-out. The FullDMD cabinets show the DMD on
/// their own ScoreView window over the `DMDImage` canvas, and the ini windows
/// sit on that window: mapping them into the backglass image then goes through
/// the ScreenRes.txt DMD area.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum IniTarget {
    /// the ini windows are placed on the backglass (the classic grill era)
    #[default]
    Backglass,
    /// the ini windows are placed on the ScoreView (FullDMD) window
    ScoreView,
}

impl Display for IniTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            IniTarget::Backglass => "backglass",
            IniTarget::ScoreView => "scoreview",
        };
        f.write_str(s)
    }
}

impl FromStr for IniTarget {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "backglass" => Ok(IniTarget::Backglass),
            "scoreview" => Ok(IniTarget::ScoreView),
            other => Err(format!("unknown ini target {other} (backglass, scoreview)")),
        }
    }
}

/// How to derive the DMD rectangle.
#[derive(Clone, Copy, PartialEq, Debug, Default, Serialize)]
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

impl Display for Method {
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

impl FromStr for Method {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "hole" => Ok(Method::Hole),
            "ini" => Ok(Method::Ini),
            "auto" => Ok(Method::Auto),
            "template" => Ok(Method::Template),
            _ => Err(format!("unknown method: {s}")),
        }
    }
}

/// Which aspect ratio the stamped rectangle should get.
///
/// The sources disagree by more than rounding, so the ratio has to be chosen
/// rather than derived: the visible glass hole is wider than the DMD content
/// (Attack from Mars: 4.74 for the hole vs the ROM's 4:1), and the ini window
/// is narrower than both (3.44).
#[derive(Clone, Copy, PartialEq, Debug, Default, Serialize)]
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

impl Display for Aspect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Aspect::Hole => "hole",
            Aspect::Native => "native",
            Aspect::Ini => "ini",
        };
        f.write_str(s)
    }
}

impl FromStr for Aspect {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "hole" => Ok(Aspect::Hole),
            "native" => Ok(Aspect::Native),
            "ini" => Ok(Aspect::Ini),
            _ => Err(format!("unknown aspect: {s}")),
        }
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

impl Display for Grid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}x{}", self.cols, self.rows)
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

/// First candidate that exists and is not empty (prepended `extra` candidates
/// first), else the first candidate.
pub fn default_ini(extra: &[PathBuf]) -> PathBuf {
    extra
        .iter()
        .cloned()
        .chain(INI_CANDIDATES.iter().map(PathBuf::from))
        .find(|p| std::fs::metadata(p).map(|m| m.len() > 0).unwrap_or(false))
        .unwrap_or_else(|| PathBuf::from(INI_CANDIDATES[0]))
}
