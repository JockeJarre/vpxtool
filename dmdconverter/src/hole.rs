//! Finds the DMD cut-out ("hole") inside an embedded B2S image.
//!
//! Port of `find_hole` from `vpxtool/src/backglass.rs` (Apache-2.0), which
//! itself is used by vpxtool's `auto_position_dmd` for the reverse direction
//! (B2S image -> DMD window rect). The algorithm: seed a grid of points, from
//! each seed trace outwards in the 4 axis directions and the 4 diagonals while
//! the colour stays within a deviation, keep the largest resulting rectangle.

use image::{Rgba, RgbaImage};

use crate::Rect;

/// Parameters used by vpxtool: 6x6 seeds, colour deviation of 5 per channel.
/// The minimum width is a fraction of the image width: vpxtool uses 1/2 for the
/// grill strip images, where the hole spans ~85 % of the width, while a
/// backglass image carries a hole of ~50 % of its width.
pub const DIVISIONS: u32 = 6;
pub const MAX_DEVIATION_U8: u8 = 5;
/// The tracer keeps any flat rectangle from 2:1 to 9:1. The upper bound is the
/// shape of a raw 128x16 DMD (8:1), not of the window cut into a backglass.
pub const MIN_TRACE_ASPECT: f64 = 2.0;
pub const MAX_TRACE_ASPECT: f64 = 9.0;

/// The shape of the DMD *window* in a backglass image. The DMD-shaped `<DMD>`
/// tags of one installation are 2.4-5.9 and the holes detected on the files
/// that carry them are also 2.4-5.9; 6.0 leaves room for both. Flat bands of
/// art are 6.2-8.9, so this is what separates a cut-out from a coloured stripe.
pub const MIN_DMD_ASPECT: f64 = 2.0;
pub const MAX_DMD_ASPECT: f64 = 6.0;

/// A DMD window is at least ~7 % of the backglass height; the false detections
/// on files with no DMD are 4.7-7.4 % high, the real ones are 7.5-15 %. 6 %
/// rejects the thin bands without touching a real cut-out.
pub const MIN_HEIGHT_FRAC: f64 = 0.06;

/// The single best candidate, the shape of vpxtool's `find_hole`. The scan uses
/// [`find_holes`] plus [`pick_hole`], so this one is the test fixture entry
/// point for the rectangle tracing itself.
#[cfg(test)]
pub fn find_hole(img: &RgbaImage, divisions: u32, min_width: f64) -> Option<Rect> {
    find_holes(img, divisions, min_width)
        .into_iter()
        .max_by_key(|r| (r.w * r.h) as i64)
}

/// Every distinct hole candidate found, biggest area first.
pub fn find_holes(img: &RgbaImage, divisions: u32, min_width: f64) -> Vec<Rect> {
    let (iw, ih) = (img.width(), img.height());
    let mut found: Vec<Rect> = Vec::new();
    for gy in 0..divisions {
        for gx in 0..divisions {
            let x1 = (gx as f64 / divisions as f64) * iw as f64;
            let x2 = ((gx + 1) as f64 / divisions as f64) * iw as f64;
            let y1 = (gy as f64 / divisions as f64) * ih as f64;
            let y2 = ((gy + 1) as f64 / divisions as f64) * ih as f64;
            let cx = (((x1 + x2) / 2.0) as u32).min(iw - 1);
            let cy = (((y1 + y2) / 2.0) as u32).min(ih - 1);
            let hole = find_hole_from(img, cx, cy, MAX_DEVIATION_U8);
            if hole.w < min_width || !(MIN_TRACE_ASPECT..=MAX_TRACE_ASPECT).contains(&hole.aspect())
            {
                continue;
            }
            if !found.contains(&hole) {
                found.push(hole);
            }
        }
    }
    // biggest area first
    found.sort_by_key(|b| std::cmp::Reverse((b.w * b.h) as i64));
    found
}

/// The uniform-colour rectangle around `(cx, cy)`.
pub fn find_hole_from(img: &RgbaImage, cx: u32, cy: u32, deviation: u8) -> Rect {
    let (iw, ih) = (img.width(), img.height());
    let center = *img.get_pixel(cx, cy);

    let mut left = cx;
    while left > 0 {
        left -= 1;
        if !within(img.get_pixel(left, cy), &center, deviation) {
            left += 1;
            break;
        }
    }
    let mut right = cx;
    while right < iw - 1 {
        right += 1;
        if !within(img.get_pixel(right, cy), &center, deviation) {
            right -= 1;
            break;
        }
    }
    let mut top = cy;
    while top > 0 {
        top -= 1;
        if !within(img.get_pixel(cx, top), &center, deviation) {
            top += 1;
            break;
        }
    }
    let mut bottom = cy;
    while bottom < ih - 1 {
        bottom += 1;
        if !within(img.get_pixel(cx, bottom), &center, deviation) {
            bottom -= 1;
            break;
        }
    }

    // Trace to the corners as well, to shrink away rounded / filleted corners.
    let center_point = (cx, cy);
    let tl = trace(img, center_point, (left, top), &center, deviation);
    let tr = trace(img, center_point, (right, top), &center, deviation);
    let bl = trace(img, center_point, (left, bottom), &center, deviation);
    let br = trace(img, center_point, (right, bottom), &center, deviation);

    let left = tl.0.max(bl.0);
    let right = tr.0.min(br.0);
    let top = tl.1.max(tr.1);
    let bottom = bl.1.min(br.1);
    Rect::new(
        left as f64,
        top as f64,
        (right - left + 1) as f64,
        (bottom - top + 1) as f64,
    )
}

fn trace(
    img: &RgbaImage,
    from: (u32, u32),
    to: (u32, u32),
    color: &Rgba<u8>,
    deviation: u8,
) -> (u32, u32) {
    let mut current = to;
    for (x, y) in LineIter::new(from, to) {
        if !within(img.get_pixel(x, y), color, deviation) {
            break;
        }
        current = (x, y);
    }
    current
}

fn within(a: &Rgba<u8>, b: &Rgba<u8>, max_deviation: u8) -> bool {
    let total: u32 =
        a.0.iter()
            .zip(b.0.iter())
            .map(|(x, y)| x.abs_diff(*y) as u32)
            .sum();
    total <= max_deviation as u32 * 4
}

struct LineIter {
    x0: i64,
    y0: i64,
    x1: i64,
    y1: i64,
    dx: i64,
    dy: i64,
    sx: i64,
    sy: i64,
    err: i64,
    done: bool,
}

impl LineIter {
    fn new(from: (u32, u32), to: (u32, u32)) -> Self {
        let (x0, y0) = (from.0 as i64, from.1 as i64);
        let (x1, y1) = (to.0 as i64, to.1 as i64);
        let dx = (x1 - x0).abs();
        let dy = (y1 - y0).abs();
        LineIter {
            x0,
            y0,
            x1,
            y1,
            dx,
            dy,
            sx: if x0 < x1 { 1 } else { -1 },
            sy: if y0 < y1 { 1 } else { -1 },
            err: dx - dy,
            done: false,
        }
    }
}

impl Iterator for LineIter {
    type Item = (u32, u32);

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let point = (self.x0 as u32, self.y0 as u32);
        if self.x0 == self.x1 && self.y0 == self.y1 {
            self.done = true;
        } else {
            let e2 = 2 * self.err;
            if e2 > -self.dy {
                self.err -= self.dy;
                self.x0 += self.sx;
            }
            if e2 < self.dx {
                self.err += self.dx;
                self.y0 += self.sy;
            }
        }
        Some(point)
    }
}

/// Decodes `bytes` and returns the hole candidates (biggest first) plus the
/// image size. `min_width_frac` is the hole width as a fraction of the image
/// width - 1/3 for a backglass image, 1/2 for a grill strip image.
pub fn find_holes_in_bytes(
    bytes: &[u8],
    min_width_frac: f64,
) -> Result<(Vec<Rect>, (u32, u32)), String> {
    let img = image::load_from_memory(bytes).map_err(|e| format!("decode image: {e}"))?;
    let rgba = img.to_rgba8();
    let size = (rgba.width(), rgba.height());
    let min_width = rgba.width() as f64 * min_width_frac;
    Ok((find_holes(&rgba, DIVISIONS, min_width), size))
}

/// Picks the candidate that sits in the grill band at the bottom of the
/// backglass image: at least 60 % of its height must be inside
/// `[image_h - grill, image_h]`. This is what keeps a flat-coloured area of the
/// playfield art from being mistaken for the DMD cut-out.
pub fn pick_in_grill(candidates: &[Rect], image_h: u32, grill: f64) -> Option<Rect> {
    let top = image_h as f64 - grill;
    candidates.iter().copied().find(|r| {
        let inside = (r.y + r.h).min(image_h as f64) - r.y.max(top);
        inside > 0.6 * r.h
    })
}

/// Where the authoring tools put the DMD box, as fractions of the backglass
/// image. Measured over the authored `<DMD>` tags of one installation: x
/// 0.23-0.25, y 0.79-0.83, width 0.51-0.53, ratio 4:1 (one 4.74).
pub const TEMPLATE_X: f64 = 0.242;
pub const TEMPLATE_Y: f64 = 0.81;
pub const TEMPLATE_W: f64 = 0.51;
pub const TEMPLATE_ASPECT: f64 = 4.0;

/// The authoring template as a rectangle: the position the B2S authoring tools
/// draw, for files where neither a hole nor the ini says anything. A plausible
/// default, not a measurement, so it is only used when asked for.
pub fn template_rect(image_w: u32, image_h: u32) -> Rect {
    let w = image_w as f64 * TEMPLATE_W;
    Rect::new(
        image_w as f64 * TEMPLATE_X,
        image_h as f64 * TEMPLATE_Y,
        w,
        w / TEMPLATE_ASPECT,
    )
}

/// Is a candidate a DMD window at all?
///
/// The cut-out is in the lower half of the backglass, does not span the whole
/// width, and starts in the left half: the authored tags of one installation sit
/// at x 0.23-0.25, y 0.79-0.83, width 0.51-0.53 of the image. The bounds here
/// are that window with room to spare, and they reject the flat-colour regions
/// that the seed grid finds in the art itself - a band across the full width, a
/// light strip at the top edge, and, most importantly, the thin stripes that
/// look like a DMD to the tracer but are only 5 % of the image high and 6-9:1
/// wide. Those are the false positives on backglasses of games that have no DMD
/// at all.
pub fn is_plausible(r: Rect, image_w: u32, image_h: u32) -> bool {
    let (w, h) = (image_w as f64, image_h as f64);
    let (xf, yf, wf) = (r.x / w, r.y / h, r.w / w);
    let aspect = r.aspect();
    r.w >= 0.05 * w
        && r.h >= MIN_HEIGHT_FRAC * h
        && (MIN_DMD_ASPECT..=MAX_DMD_ASPECT).contains(&aspect)
        && (0.25..=0.90).contains(&wf)
        && xf <= 0.60
        && (0.45..=1.0).contains(&yf)
}

/// The hole to stamp, out of the candidates of one backglass image.
///
/// Candidates that cannot be a cut-out are dropped first, then the grill band
/// wins if a candidate is in it, and of the rest the one closest to the
/// authoring template is taken - that is a better ranking than "biggest",
/// because a flat area of art is often bigger than the real hole.
pub fn pick_hole(candidates: &[Rect], image_w: u32, image_h: u32, grill: f64) -> Option<Rect> {
    let plausible: Vec<Rect> = candidates
        .iter()
        .copied()
        .filter(|r| is_plausible(*r, image_w, image_h))
        .collect();
    if plausible.is_empty() {
        return None;
    }
    let pool = if grill > 0.0 {
        match pick_in_grill(&plausible, image_h, grill) {
            Some(in_band) => vec![in_band],
            None => plausible,
        }
    } else {
        plausible
    };
    let distance = |r: &Rect| {
        let dx = (r.x / image_w as f64 - TEMPLATE_X).abs();
        let dy = (r.y / image_h as f64 - TEMPLATE_Y).abs();
        let dw = (r.w / image_w as f64 - TEMPLATE_W).abs();
        dx + dy + dw
    };
    pool.iter()
        .min_by(|a, b| distance(a).total_cmp(&distance(b)))
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(w: u32, h: u32) -> RgbaImage {
        let mut img = RgbaImage::new(w, h);
        for (x, y, px) in img.enumerate_pixels_mut() {
            *px = Rgba([
                ((x * 7 + y * 13) % 251) as u8,
                ((x * 11 + y * 5) % 241) as u8,
                ((x * 3 + y * 17) % 233) as u8,
                255,
            ]);
        }
        img
    }

    fn square(img: &mut RgbaImage, x: u32, y: u32, w: u32, h: u32, color: Rgba<u8>) {
        for px in x..x + w {
            for py in y..y + h {
                img.put_pixel(px, py, color);
            }
        }
    }

    #[test]
    fn finds_a_flat_rectangle() {
        let mut img = noise(320, 200);
        square(&mut img, 100, 50, 200, 50, Rgba([255, 170, 34, 255]));
        let hole = find_hole(&img, 10, 50.0).unwrap();
        assert_eq!((hole.x as u32, hole.y as u32), (100, 50));
        assert_eq!((hole.w as u32, hole.h as u32), (200, 50));
    }

    #[test]
    fn ignores_noise_only_images() {
        let img = noise(320, 200);
        assert!(find_hole(&img, 10, 100.0).is_none());
    }

    #[test]
    fn square_blobs_are_not_dmd_holes() {
        let mut img = noise(320, 200);
        square(&mut img, 10, 10, 120, 120, Rgba([10, 10, 10, 255]));
        assert!(find_holes(&img, 8, 50.0).is_empty());
    }

    #[test]
    fn grill_band_prefers_the_hole_in_the_strip() {
        // backglass 320x200, grill 60 -> band [140,200]; playfield blob at y 10
        let mut img = noise(320, 200);
        square(&mut img, 20, 10, 200, 60, Rgba([255, 170, 34, 255]));
        square(&mut img, 80, 150, 160, 40, Rgba([9, 9, 9, 255]));
        let cands = find_holes(&img, 8, 320.0 / 3.0);
        assert_eq!(cands.len(), 2);
        let picked = pick_in_grill(&cands, 200, 60.0).unwrap();
        assert_eq!((picked.x as u32, picked.y as u32), (80, 150));
    }

    #[test]
    fn the_plausibility_window_rejects_where_no_dmd_can_be() {
        // 2000x1565, the size of a common backglass image
        // the authored position: x 0.24, y 0.81, width 0.51
        assert!(is_plausible(
            Rect::new(484.0, 1268.0, 1034.0, 218.0),
            2000,
            1565
        ));
        // a band across the whole width is art, not a cut-out
        assert!(!is_plausible(
            Rect::new(0.0, 2276.0, 2987.0, 620.0),
            3000,
            2896
        ));
        // a light strip at the top edge
        assert!(!is_plausible(
            Rect::new(419.0, 0.0, 2165.0, 257.0),
            3000,
            2728
        ));
        // right half of the image
        assert!(!is_plausible(
            Rect::new(1290.0, 5.0, 850.0, 327.0),
            2500,
            2150
        ));
        // too narrow to be the DMD
        assert!(!is_plausible(
            Rect::new(500.0, 1200.0, 60.0, 200.0),
            2000,
            1565
        ));
    }

    #[test]
    fn thin_art_bands_are_not_dmd_windows() {
        // the false detections from backglasses of games without a DMD: a flat
        // stripe 33-53 % wide, 4.7-7.4 % high, 6.2-8.9:1
        let cases = [
            // Bad Girls (Gottlieb 1988), 1500x1200
            (Rect::new(448.0, 1053.0, 615.0, 84.0), 1500, 1200),
            // Raven (Gottlieb 1986), 1280x1024
            (Rect::new(405.0, 741.0, 471.0, 76.0), 1280, 1024),
            // Check Mate (Taito do Brasil 1977), 2100x1921
            (Rect::new(149.0, 1085.0, 794.0, 104.0), 2100, 1921),
            // Space Patrol (Taito do Brasil 1978), 2100x1921
            (Rect::new(154.0, 1093.0, 792.0, 102.0), 2100, 1921),
        ];
        for (rect, w, h) in cases {
            assert!(
                !is_plausible(rect, w, h),
                "false band {rect} on {w}x{h} was accepted"
            );
        }

        // a thin band can also stay under the aspect limit and still be art;
        // the height bound is what rejects it
        let thin = Rect::new(500.0, 1200.0, 400.0, 80.0);
        assert!((thin.aspect() - 5.0).abs() < 0.01);
        assert!(!is_plausible(thin, 2000, 1565));

        // and a real hole at the widest ratio seen on an authored file passes
        assert!(is_plausible(
            Rect::new(335.0, 1005.0, 589.0, 102.0),
            2000,
            1565
        ));
    }

    #[test]
    fn pick_prefers_the_candidate_at_the_authored_place() {
        // a flat area of the art is bigger than the real hole, so "biggest" is
        // the wrong ranking; the one near the authored position must win
        let art = Rect::new(300.0, 800.0, 1400.0, 400.0);
        let real = Rect::new(484.0, 1268.0, 1034.0, 218.0);
        assert_eq!(
            pick_hole(&[art, real], 2000, 1565, 0.0),
            Some(real),
            "the larger flat area won"
        );
        // and nothing at all when every candidate is out of the window
        assert_eq!(
            pick_hole(&[Rect::new(0.0, 0.0, 2000.0, 300.0)], 2000, 1565, 0.0),
            None
        );
    }

    #[test]
    fn the_template_is_the_authored_box() {
        let r = template_rect(2000, 1565);
        assert!(r.w > 1000.0 && r.w < 1100.0, "width {}", r.w);
        assert!(
            (r.aspect() - TEMPLATE_ASPECT).abs() < 0.01,
            "ratio {}",
            r.aspect()
        );
        assert!(is_plausible(r, 2000, 1565), "the template itself must pass");
    }
}
