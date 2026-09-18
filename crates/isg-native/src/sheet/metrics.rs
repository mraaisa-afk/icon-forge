//! §3.5 metrics — what an icon's ink looks like, measured from its mask.
//!
//! Auto-leveling needs four numbers per icon, and all four are properties of
//! the *ink*, not of the vector geometry: how big the ink is, where its mass
//! sits, how thick it is, and how solid it looks. Reading them from the mask
//! (rather than from the traced path) is what makes the result independent of
//! how faithfully a particular preset happened to trace that icon.
//!
//! Everything here is pure: a [`ForegroundMask`] and a bbox in, numbers out. No
//! image decoding, no allocation beyond the icon's own pixels, and no external
//! crate — so the arithmetic can be tested without a corpus and the same
//! numbers are available on any platform.
//!
//! * **ink box / area / centroid** — a scan of the crop.
//! * **stroke weight** — `2 × max inscribed radius`, i.e. twice the largest
//!   distance-transform value inside the ink (§3.5). A 4 px outline gives 4, a
//!   filled square gives its shorter side.
//! * **solidity** — ink area over the area of its convex hull. A filled square
//!   scores 1.0, a ring or a cross much less, which is exactly the "looks
//!   lighter than it measures" signal the leveling formula damps.

use isg_core::{Bbox, ForegroundMask};

/// Chunky pixels per unit: the distance transform is kept in integer
/// millipixels so the two-pass chamfer is exact and deterministic. This is the
/// same scale the Phase-3 splitter uses (`DT_SCALE`).
const DT_SCALE: i32 = 1000;

/// Maximum number of hull vertices kept. A monotone chain over an icon-sized
/// blob produces a handful; the cap only exists to bound the allocation.
const MAX_HULL: usize = 1024;

/// One icon's ink, measured inside its own crop.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IconMetrics {
    /// Left edge of the ink's bounding box, in crop-local pixels.
    pub ink_x: f32,
    /// Top edge of the ink's bounding box, in crop-local pixels.
    pub ink_y: f32,
    /// Width of the ink's bounding box, in pixels (`> 0`).
    pub ink_w: f32,
    /// Height of the ink's bounding box, in pixels (`> 0`).
    pub ink_h: f32,
    /// Foreground pixel count.
    pub ink_area: u32,
    /// Centre of mass of the ink, in crop-local pixels.
    pub centroid_x: f32,
    /// Centre of mass of the ink, in crop-local pixels.
    pub centroid_y: f32,
    /// Stroke weight: `2 × max inscribed radius`, in pixels (`> 0`).
    pub stroke: f32,
    /// Ink area over convex-hull area, in `(0, 1]`.
    pub solidity: f32,
}

impl IconMetrics {
    /// The ink box's centre, in crop-local pixels.
    #[must_use]
    pub fn ink_center(&self) -> (f32, f32) {
        (self.ink_x + self.ink_w * 0.5, self.ink_y + self.ink_h * 0.5)
    }

    /// The larger side of the ink box — the size a containment fit normalizes.
    #[must_use]
    pub fn ink_long_side(&self) -> f32 {
        self.ink_w.max(self.ink_h)
    }
}

/// Measures the ink of one icon from its mask and its crop rectangle.
///
/// Returns `None` when the crop holds no foreground pixels at all (the caller
/// treats that as an icon with nothing to place, not as an error).
#[must_use]
pub fn measure(mask: &ForegroundMask, bbox: Bbox) -> Option<IconMetrics> {
    if bbox.w == 0 || bbox.h == 0 {
        return None;
    }
    let w = bbox.w as usize;
    let h = bbox.h as usize;
    let mut bits = vec![false; w * h];
    let mut area: u32 = 0;
    let mut sum_x: u64 = 0;
    let mut sum_y: u64 = 0;
    let mut min_x = usize::MAX;
    let mut min_y = usize::MAX;
    let mut max_x = 0usize;
    let mut max_y = 0usize;
    for y in 0..h {
        for x in 0..w {
            if !mask.get(bbox.x + x as u32, bbox.y + y as u32) {
                continue;
            }
            bits[y * w + x] = true;
            area += 1;
            sum_x += x as u64;
            sum_y += y as u64;
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }
    }
    if area == 0 {
        return None;
    }
    let ink_x = min_x as f32;
    let ink_y = min_y as f32;
    let ink_w = (max_x - min_x + 1) as f32;
    let ink_h = (max_y - min_y + 1) as f32;
    let area_f = area as f32;
    let centroid_x = sum_x as f32 / area_f;
    let centroid_y = sum_y as f32 / area_f;
    let stroke = 2.0 * max_inscribed_radius(w, h, &bits);
    // Solidity is ink over hull, so `1.0` means "the ink *is* its hull" — true
    // of any rectangle, which is what most icons are, and false of anything
    // with a hole, a notch or a diagonal. A hull is never smaller than its ink,
    // and the clamp only guards a degenerate hull (one or two pixels).
    let solidity = match hull_area(w, h, &bits) {
        Some(hull) if hull > 0.0 => (area_f / hull).min(1.0),
        _ => 1.0,
    };
    Some(IconMetrics {
        ink_x,
        ink_y,
        ink_w,
        ink_h,
        ink_area: area,
        centroid_x,
        centroid_y,
        stroke,
        solidity,
    })
}

/// The largest distance from an ink pixel to the nearest background pixel
/// (in pixels), by two-pass chamfer.
fn max_inscribed_radius(w: usize, h: usize, bits: &[bool]) -> f32 {
    if w == 0 || h == 0 {
        return 0.0;
    }
    const ORTH: i32 = DT_SCALE;
    const DIAG: i32 = 1414; // ≈ sqrt(2)·1000, the usual chamfer constant
    let far = i32::MAX / 4;
    let mut dt = vec![if bits[0] { far } else { 0 }; w * h];
    for i in 0..w * h {
        dt[i] = if bits[i] { far } else { 0 };
    }
    // Forward pass: north, west, north-west, north-east.
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if !bits[i] {
                continue;
            }
            let mut best = dt[i];
            if y > 0 {
                best = best.min(dt[i - w] + ORTH);
                if x > 0 {
                    best = best.min(dt[i - w - 1] + DIAG);
                }
                if x + 1 < w {
                    best = best.min(dt[i - w + 1] + DIAG);
                }
            }
            if x > 0 {
                best = best.min(dt[i - 1] + ORTH);
            }
            dt[i] = best;
        }
    }
    // Backward pass: south, east, south-east, south-west.
    for y in (0..h).rev() {
        for x in (0..w).rev() {
            let i = y * w + x;
            if !bits[i] {
                continue;
            }
            let mut best = dt[i];
            if y + 1 < h {
                best = best.min(dt[i + w] + ORTH);
                if x + 1 < w {
                    best = best.min(dt[i + w + 1] + DIAG);
                }
                if x > 0 {
                    best = best.min(dt[i + w - 1] + DIAG);
                }
            }
            if x + 1 < w {
                best = best.min(dt[i + 1] + ORTH);
            }
            dt[i] = best;
        }
    }
    let max = dt.iter().copied().max().unwrap_or(0);
    if max >= far {
        // Every pixel is ink (a solid crop): the inscribed radius is bounded by
        // the crop itself, half of its shorter side, not by "infinity".
        return w.min(h) as f32 * 0.5;
    }
    max as f32 / DT_SCALE as f32
}

/// Area of the convex hull of the ink, in ink space: the hull of the ink
/// pixels' centres, grown by half a pixel in every direction — that is, the
/// Minkowski sum of the centre hull with a unit square, since every ink pixel
/// covers a unit square.
///
/// Growing it is what makes the number meaningful: without it the hull of a 4×4
/// square of pixels is 3×3 and a solid icon would score 16/9. With it, the
/// square scores exactly 1.0 and only real concavity (a ring, a notch, a
/// diagonal) pulls the score down.
///
/// `None` when the boundary has fewer than three distinct points (a single
/// pixel, a 1-pixel diagonal), where a hull has no area of its own.
fn hull_area(w: usize, h: usize, bits: &[bool]) -> Option<f32> {
    // The hull of a blob is the hull of its extreme pixels; collecting the
    // boundary saves sorting the interior. A pixel is on the boundary when any
    // 4-neighbour is outside the ink box or is background.
    let mut points: Vec<(i32, i32)> = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if !bits[i] {
                continue;
            }
            let edge = x == 0
                || y == 0
                || x + 1 == w
                || y + 1 == h
                || !bits[i - 1]
                || !bits[i + 1]
                || !bits[i - w]
                || !bits[i + w];
            if edge {
                points.push((x as i32, y as i32));
            }
        }
    }
    if points.len() < 3 {
        return None;
    }
    points.sort_unstable();
    points.dedup();
    let hull = monotone_chain(&points);
    if hull.len() < 3 {
        return None;
    }
    let mut twice_area = 0i64;
    let mut perimeter = 0f64;
    for i in 0..hull.len() {
        let (x0, y0) = hull[i];
        let (x1, y1) = hull[(i + 1) % hull.len()];
        twice_area += i64::from(x0) * i64::from(y1) - i64::from(x1) * i64::from(y0);
        let (dx, dy) = (f64::from(x1 - x0), f64::from(y1 - y0));
        perimeter += (dx * dx + dy * dy).sqrt();
    }
    let area = (twice_area.abs() as f64) * 0.5;
    // Area(P ⊕ K) = area(P) + perimeter(P)·r + area(K), with r = 0.5 and K the
    // unit square: a 3×3 centre hull becomes the 4×4 ink it stands for.
    Some((area + perimeter * 0.5 + 1.0) as f32)
}

/// Andrew's monotone chain; returns the hull in counter-clockwise order.
fn monotone_chain(points: &[(i32, i32)]) -> Vec<(i32, i32)> {
    let cross = |o: (i32, i32), a: (i32, i32), b: (i32, i32)| -> i64 {
        let (ox, oy) = o;
        let (ax, ay) = a;
        let (bx, by) = b;
        i64::from(ax - ox) * i64::from(by - oy) - i64::from(ay - oy) * i64::from(bx - ox)
    };
    let mut hull: Vec<(i32, i32)> = Vec::with_capacity(MAX_HULL.min(points.len() * 2 + 2));
    for &p in points {
        while hull.len() >= 2 && cross(hull[hull.len() - 2], hull[hull.len() - 1], p) <= 0 {
            hull.pop();
        }
        hull.push(p);
    }
    let lower = hull.len() + 1;
    for &p in points.iter().rev().skip(1) {
        while hull.len() >= lower && cross(hull[hull.len() - 2], hull[hull.len() - 1], p) <= 0 {
            hull.pop();
        }
        hull.push(p);
    }
    if hull.len() > 1 {
        hull.pop(); // the first point is repeated at the end
    }
    hull
}

#[cfg(test)]
mod tests {
    use super::*;
    use isg_core::ForegroundMask;

    fn mask_from(rows: &[&str]) -> (ForegroundMask, Bbox) {
        let h = rows.len() as u32;
        let w = rows[0].len() as u32;
        let mut mask = ForegroundMask::new(w, h);
        for (y, row) in rows.iter().enumerate() {
            for (x, c) in row.chars().enumerate() {
                if c == '#' {
                    mask.set(x as u32, y as u32, true);
                }
            }
        }
        (mask, Bbox { x: 0, y: 0, w, h })
    }

    #[test]
    fn measures_a_filled_rectangle() {
        let (mask, bbox) = mask_from(&["......", ".####.", ".####.", ".####.", "......"]);
        let m = measure(&mask, bbox).expect("ink");
        assert_eq!((m.ink_x, m.ink_y), (1.0, 1.0));
        assert_eq!((m.ink_w, m.ink_h), (4.0, 3.0));
        assert_eq!(m.ink_area, 12);
        assert_eq!(m.centroid_x, 2.5);
        assert_eq!(m.centroid_y, 2.0);
        // The largest inscribed disc has radius 2 px — half the shorter side.
        assert!((m.stroke - 4.0).abs() < 0.1, "stroke {}", m.stroke);
        // A rectangle is its own hull, so it scores exactly 1.0.
        assert!((m.solidity - 1.0).abs() < 1e-6, "solidity {}", m.solidity);
    }

    #[test]
    fn a_ring_is_thin_and_pays_for_its_hole() {
        let (mask, bbox) = mask_from(&[
            ".........",
            ".#######.",
            ".#.....#.",
            ".#.....#.",
            ".#.....#.",
            ".#######.",
            ".........",
        ]);
        let m = measure(&mask, bbox).expect("ink");
        assert_eq!((m.ink_w, m.ink_h), (7.0, 5.0));
        // 2 px thick outline ⇒ the inscribed radius is about 1 px.
        assert!((m.stroke - 2.0).abs() < 0.2, "stroke {}", m.stroke);
        // The hull of a closed ring is its box (20 px² of centre hull, grown to
        // ~30) against 18 px of ink: solid, but visibly not a filled shape.
        assert!((0.5..0.7).contains(&m.solidity), "solidity {}", m.solidity);
    }

    #[test]
    fn a_solid_crop_does_not_run_off_the_distance_transform() {
        let (mask, bbox) = mask_from(&["####", "####", "####"]);
        let m = measure(&mask, bbox).expect("ink");
        // No background at all: the inscribed radius is bounded by the crop —
        // half of its shorter side, so a 3-px-tall crop gives 3.
        assert!((m.stroke - 3.0).abs() < 1e-6, "stroke {}", m.stroke);
        assert!((m.solidity - 1.0).abs() < 1e-6);
    }

    #[test]
    fn an_empty_crop_measures_nothing() {
        let (mask, bbox) = mask_from(&["...", "...", "..."]);
        assert!(measure(&mask, bbox).is_none());
        let (mask, _) = mask_from(&["..."]);
        assert!(measure(
            &mask,
            Bbox {
                x: 0,
                y: 0,
                w: 0,
                h: 3
            }
        )
        .is_none());
    }

    #[test]
    fn measures_inside_a_crop_that_is_not_at_the_origin() {
        let mut mask = ForegroundMask::new(20, 20);
        for y in 10..14 {
            for x in 6..16 {
                mask.set(x, y, true);
            }
        }
        let m = measure(
            &mask,
            Bbox {
                x: 5,
                y: 9,
                w: 12,
                h: 6,
            },
        )
        .expect("ink");
        // Local coordinates: the crop starts at (5, 9).
        assert_eq!((m.ink_x, m.ink_y), (1.0, 1.0));
        assert_eq!((m.ink_w, m.ink_h), (10.0, 4.0));
        assert_eq!(m.ink_area, 40);
        assert_eq!(m.ink_center(), (6.0, 3.0));
    }

    #[test]
    fn a_diagonal_blob_has_no_hull_of_its_own_but_still_scores() {
        // Two pixels touching corner to corner: the centre hull is a segment
        // with no area, so solidity falls back to 1.0 rather than NaN.
        let (mask, bbox) = mask_from(&["#.", ".#"]);
        let m = measure(&mask, bbox).expect("ink");
        assert_eq!(m.ink_area, 2);
        assert!((m.solidity - 1.0).abs() < 1e-6, "{}", m.solidity);
    }

    #[test]
    fn a_notched_shape_is_less_solid_than_its_own_box() {
        // An L: 12 px of ink in a 4×4 box, missing a 2×2 corner.
        let (mask, bbox) = mask_from(&["####", "####", "##..", "##.."]);
        let m = measure(&mask, bbox).expect("ink");
        let (rect_mask, rect_bbox) = mask_from(&["####", "####", "####", "####"]);
        let rect = measure(&rect_mask, rect_bbox).expect("ink");
        assert_eq!(m.ink_area, 12);
        assert_eq!(rect.ink_area, 16);
        assert!((rect.solidity - 1.0).abs() < 1e-6, "rect {}", rect.solidity);
        // The hull of the L's pixel centres is 7 px², grown to ≈13.4 by the
        // unit square each pixel stands for, against 12 px of ink — so the
        // notch costs about a tenth, while a 2 px ring loses a third (§ tests
        // above). That is the sensitivity the leveling formula is tuned for.
        assert!(
            (0.88..0.91).contains(&m.solidity),
            "solidity {}",
            m.solidity
        );
        assert!(m.solidity < rect.solidity);
    }
}
