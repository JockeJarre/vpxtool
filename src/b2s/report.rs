//! Human readable per-table report plus the run summary.

use crate::b2s::Aspect;
use crate::b2s::scan::{MatchKind, Record};
use std::io::Write;

pub fn print(records: &[Record]) {
    let mut out = String::new();
    let mut exact = 0;
    let mut similar = 0;
    let mut ambiguous = 0;
    let mut missing = 0;
    let mut roms = 0;
    let mut sections = 0;
    let mut holes = 0;
    let mut stamped = 0;
    let mut from_hole = 0;
    let mut from_ini = 0;
    let mut from_tag = 0;
    let mut from_template = 0;

    for rec in records {
        match rec.match_kind {
            MatchKind::Exact => exact += 1,
            MatchKind::Similar => similar += 1,
            MatchKind::Ambiguous => ambiguous += 1,
            MatchKind::Missing => missing += 1,
        }
        out.push_str(&format!("\n{}\n", rec.vpx));
        match rec.match_kind {
            MatchKind::Exact => out.push_str(&format!(
                "  b2s   {}\n",
                rel(rec.b2s.as_deref().unwrap_or("?"))
            )),
            MatchKind::Similar => out.push_str(&format!(
                "  b2s   {} [similar: {}]\n",
                rel(rec.b2s.as_deref().unwrap_or("?")),
                rec.b2s.as_deref().map(table_name).unwrap_or_default()
            )),
            MatchKind::Ambiguous => out.push_str(&format!(
                "  b2s   {} [ambiguous]\n",
                rel(rec.b2s.as_deref().unwrap_or("?"))
            )),
            MatchKind::Missing => out.push_str("  b2s   -\n"),
        }
        if let Some(rom) = &rec.rom {
            roms += 1;
            let src = rec.rom_source.as_deref().unwrap_or("?");
            out.push_str(&format!("  rom   {rom}  ({src})\n"));
        }
        if let Some(g) = rec.grill_height {
            out.push_str(&format!("  grill {g:.0}\n"));
        }
        if let Some((w, h)) = rec.backglass_size {
            out.push_str(&format!("  image {w}x{h}\n"));
        }
        if let Some((w, h)) = rec.dmd_image_size {
            out.push_str(&format!("  dmd   {w}x{h}\n"));
        }
        if rec.hole_scanned {
            out.push_str(&format!("  hole  seen {} ", rec.hole_seen));
            match rec.hole_backglass {
                Some(r) => out.push_str(&format!(
                    "backglass {:.0},{:.0} {:.0}x{:.0}\n",
                    r.x, r.y, r.w, r.h
                )),
                None => out.push_str("none\n"),
            }
            if rec.hole_seen > 0 {
                holes += 1;
            }
        }
        if let Some(r) = rec.ini_rect {
            out.push_str(&format!(
                "  ini   {}\n",
                match rec.ini_section {
                    true => format!(
                        "[{}] {:.0},{:.0} {:.0}x{:.0}",
                        rec.rom.as_deref().unwrap_or("?"),
                        r.x,
                        r.y,
                        r.w,
                        r.h
                    ),
                    false => "no section".into(),
                }
            ));
            if rec.ini_section {
                sections += 1;
            }
            if let Some(m) = rec.ini_in_image {
                out.push_str(&format!(
                    "        mapped {:.0},{:.0} {:.0}x{:.0}\n",
                    m.x, m.y, m.w, m.h
                ));
            }
        }
        if let Some(r) = rec.existing_dmd {
            out.push_str(&format!(
                "  tag   {:.0},{:.0} {:.0}x{:.0}\n",
                r.x, r.y, r.w, r.h
            ));
        }
        if let Some(r) = rec.chosen {
            stamped += 1;
            let src = rec.source.as_deref().unwrap_or("?");
            match src {
                "hole" => from_hole += 1,
                "ini" => from_ini += 1,
                "tag" => from_tag += 1,
                "template" => from_template += 1,
                _ => {}
            }
            out.push_str(&format!(
                "  STAMP {:.0},{:.0} {:.0}x{:.0}  <- {src}\n",
                r.x, r.y, r.w, r.h
            ));
            if let Some(a) = rec.chosen.map(|r| r.aspect()) {
                out.push_str(&format!("        ratio {a:.3}  "));
                out.push_str(match rec.aspect {
                    Aspect::Ini => "from ini window",
                    Aspect::Native => "from native grid",
                    Aspect::Hole => "hole, kept",
                });
                out.push('\n');
            }
        }
        for n in &rec.notes {
            out.push_str(&format!("  NOTE  {n}\n"));
        }
    }

    out.push_str("\nsummary\n");
    out.push_str(&format!("  tables               {}\n", records.len()));
    out.push_str(&format!("  exact name match     {exact}\n"));
    out.push_str(&format!("  similar name match   {similar}\n"));
    out.push_str(&format!("  ambiguous name match {ambiguous}\n"));
    out.push_str(&format!("  no b2s file          {missing}\n"));
    out.push_str(&format!("  roms found           {roms}\n"));
    out.push_str(&format!("  rom with ini section {sections}\n"));
    out.push_str(&format!("  hole detected        {holes}\n"));
    out.push_str(&format!("  stampable rect       {stamped}\n"));
    out.push_str(&format!("    from hole          {from_hole}\n"));
    out.push_str(&format!("    from ini           {from_ini}\n"));
    out.push_str(&format!("    from tag           {from_tag}\n"));
    out.push_str(&format!("    from template      {from_template}\n"));
    crate::println!("{out}").ok();
}

/// Path relative to the current directory when that is shorter, else as is.
fn rel(p: &str) -> String {
    let path = std::path::Path::new(p);
    let cwd = std::env::current_dir().unwrap_or_default();
    match path.strip_prefix(&cwd) {
        Ok(rest) if !rest.as_os_str().is_empty() => rest.display().to_string(),
        _ => path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| p.to_string()),
    }
}

fn table_name(p: &str) -> String {
    let f = p.rsplit(['/', '\\']).next().unwrap_or(p).to_string();
    let dot = f.rfind('.').unwrap_or(f.len());
    f[..dot].to_string()
}
