//! Human readable per-table report plus the run summary.

use crate::scan::{MatchKind, Record};
use crate::Aspect;

pub fn print(records: &[Record]) {
    let mut exact = 0;
    let mut similar = 0;
    let mut ambiguous = 0;
    let mut missing = 0;
    let mut stamped = 0;
    let mut roms = 0;
    let mut sections = 0;
    let mut holes = 0;
    let mut from_hole = 0;
    let mut from_ini = 0;
    let mut from_tag = 0;
    let mut from_template = 0;

    for rec in records {
        println!("\n{}", rec.vpx);
        match rec.match_kind {
            MatchKind::Exact => {
                exact += 1;
                println!("  b2s   {}", rel(rec.b2s.as_deref().unwrap_or("?")));
            }
            MatchKind::Similar => {
                similar += 1;
                println!(
                    "  b2s   {} [similar: {}]",
                    rel(rec.b2s.as_deref().unwrap_or("?")),
                    rec.b2s.as_deref().map(table_name).unwrap_or_default()
                );
            }
            MatchKind::Ambiguous => {
                ambiguous += 1;
                println!(
                    "  b2s   {} [ambiguous]",
                    rel(rec.b2s.as_deref().unwrap_or("?"))
                );
            }
            MatchKind::Missing => missing += 1,
        }

        if let Some(rom) = &rec.rom {
            roms += 1;
            let src = rec.rom_source.as_deref().unwrap_or("?");
            println!("  rom   {rom}  ({src})");
        }
        if rec.ini_section {
            sections += 1;
        }
        if let Some(g) = rec.grill_height {
            println!("  grill {g:.0}");
        }
        if let Some((w, h)) = rec.backglass_size {
            println!("  bg    {w}x{h}");
        }
        if let Some((w, h)) = rec.dmd_image_size {
            println!("  dmdimg {w}x{h}");
        }
        if let Some(r) = &rec.existing_dmd {
            println!("  tag   {r}  aspect {:.2}", r.aspect());
        }
        if let Some(r) = &rec.hole_backglass {
            holes += 1;
            println!("  hole  {r}  aspect {:.2}", r.aspect());
        }
        if let Some(r) = &rec.hole_dmd_image {
            println!("  dmdhl {r}  aspect {:.2}  (grill strip)", r.aspect());
        }
        if let Some(r) = &rec.ini_rect {
            let mapped = rec
                .ini_in_image
                .map(|m| format!("  ->  {m} in image"))
                .unwrap_or_default();
            println!("  ini   {r}  aspect {:.2}{mapped}", r.aspect());
        }
        if let Some(r) = &rec.chosen {
            let aspect = if rec.aspect == Aspect::Hole {
                String::new()
            } else {
                format!(" aspect {}", rec.aspect)
            };
            let source = rec.source.as_deref().unwrap_or("?");
            println!("  =>    {r} via {source}{aspect}");
        }
        for note in &rec.notes {
            println!("  note  {note}");
        }

        if rec.chosen.is_some() {
            stamped += 1;
        }
        match rec.source.as_deref() {
            Some("hole") => from_hole += 1,
            Some("ini") => from_ini += 1,
            Some("tag") => from_tag += 1,
            Some("template") => from_template += 1,
            _ => {}
        }
    }

    println!("\nsummary");
    println!("  tables               {}", records.len());
    println!("  exact name match     {exact}");
    println!("  similar name match   {similar}");
    println!("  ambiguous name match {ambiguous}");
    println!("  no b2s file          {missing}");
    println!("  roms found           {roms}");
    println!("  rom with ini section {sections}");
    println!("  hole detected        {holes}");
    println!("  stampable rect       {stamped}");
    println!("    from hole          {from_hole}");
    println!("    from ini           {from_ini}");
    println!("    from tag           {from_tag}");
    println!("    from template      {from_template}");
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
