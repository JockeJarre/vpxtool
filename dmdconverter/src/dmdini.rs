//! Reader for PinMAME's `DmdDevice.ini`.
//!
//! The file is a Windows profile ini: `[global]` first, then one section per
//! ROM. Keys inside a game section are prefixed with the output name
//! (`virtualdmd left = ...`), `[global]` keys are not. Keys are case
//! insensitive, values may be quoted, and sections may appear more than once
//! (later wins). All coordinates are absolute desktop pixels, floats allowed
//! (they are the signature of a DPI scale factor being divided out).

use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, PartialEq, Debug, Default)]
pub struct VirtualDmd {
    pub section_found: bool,
    pub enabled: Option<bool>,
    pub style: Option<String>,
    pub left: Option<f64>,
    pub top: Option<f64>,
    pub width: Option<f64>,
    pub height: Option<f64>,
}

impl VirtualDmd {
    pub fn rect(&self) -> Option<crate::Rect> {
        match (self.left, self.top, self.width, self.height) {
            (Some(x), Some(y), Some(w), Some(h)) => Some(crate::Rect::new(x, y, w, h)),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub struct DmdDeviceIni {
    /// where this was read from, shown in the log
    #[allow(dead_code)]
    pub path: PathBuf,
    sections: Vec<(String, Vec<(String, String)>)>,
}

impl DmdDeviceIni {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        Ok(Self {
            path: path.to_path_buf(),
            sections: parse(&text),
        })
    }

    pub fn section_count(&self) -> usize {
        self.sections.len()
    }

    /// Merges `[global]` with every section named exactly `rom` (case
    /// insensitive, later sections override), like `GetPrivateProfileString`.
    pub fn lookup(&self, rom: &str) -> VirtualDmd {
        let mut merged: Vec<(String, String)> = Vec::new();
        let mut section_found = false;
        for (name, keys) in &self.sections {
            if name == "global" || name.eq_ignore_ascii_case(rom) {
                if !name.eq_ignore_ascii_case("global") {
                    section_found = true;
                }
                for (k, v) in keys {
                    if let Some(slot) = merged.iter_mut().find(|(mk, _)| mk == k) {
                        slot.1 = v.clone();
                    } else {
                        merged.push((k.clone(), v.clone()));
                    }
                }
            }
        }
        let get = |key: &str| {
            merged
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
        };
        let num = |key: &str| get(key).and_then(|v| v.parse::<f64>().ok());
        VirtualDmd {
            section_found,
            enabled: get("virtualdmd enabled").and_then(|v| parse_bool(&v)),
            style: get("virtualdmd style"),
            left: num("virtualdmd left"),
            top: num("virtualdmd top"),
            width: num("virtualdmd width"),
            height: num("virtualdmd height"),
        }
    }
}

fn parse_bool(v: &str) -> Option<bool> {
    match v.trim().to_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn parse(text: &str) -> Vec<(String, Vec<(String, String)>)> {
    let mut sections: Vec<(String, Vec<(String, String)>)> = Vec::new();
    let mut current: Option<usize> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            let name = line[1..line.len() - 1].trim().to_lowercase();
            sections.push((name, Vec::new()));
            current = Some(sections.len() - 1);
            continue;
        }
        let Some(idx) = current else { continue };
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_lowercase();
        let value = unquote(value.trim());
        if key.is_empty() {
            continue;
        }
        sections[idx].1.push((key, value));
    }
    sections
}

fn unquote(v: &str) -> String {
    let v = v.trim();
    if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') {
        v[1..v.len() - 1].to_string()
    } else {
        v.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[global]
resize = fit
[virtualdmd]
enabled = true
[afm_113b]
virtualdmd left = 3174
virtualdmd top = 2310.5
virtualdmd width = 1322
virtualdmd height = 384
"#;

    #[test]
    fn reads_a_game_section() {
        let ini = DmdDeviceIni {
            path: PathBuf::from("test"),
            sections: parse(SAMPLE),
        };
        let dmd = ini.lookup("afm_113b");
        assert!(dmd.section_found);
        assert_eq!(dmd.left, Some(3174.0));
        assert_eq!(dmd.top, Some(2310.5));
        assert_eq!(dmd.rect().unwrap().w, 1322.0);
    }

    #[test]
    fn unknown_rom_has_no_section() {
        let ini = DmdDeviceIni {
            path: PathBuf::from("test"),
            sections: parse(SAMPLE),
        };
        assert!(!ini.lookup("nope_123").section_found);
    }
}
