//! §3.3-② Background detection — border-consensus histogram, CIE-Lab ΔE
//! masking, and the alpha → k-means → Otsu fallback ladder.
//!
//! ΔE (CIE-Lab) instead of RGB distance is the whole point: JPEG artifacts
//! are RGB outliers but perceptually identical to the background, so a ΔE
//! threshold cuts speckle sharply without touching real icons.

use isg_core::{ForegroundMask, RasterView};

use super::raster::SheetRaster;

/// How the background was established (drives [`build_mask`]'s fast path).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackgroundKind {
    /// >85% of border pixels agree in a 4-4-4 RGB bin.
    BorderConsensus,
    /// The sheet has a real alpha channel; transparency is the background.
    Alpha,
    /// Weak border consensus — k=2 k-means seeded from corners.
    KMeans,
    /// Last resort: Otsu on luma, ink = minority side of the border.
    Otsu,
}

/// Detected background description.
#[derive(Clone, Copy, Debug)]
pub struct BackgroundModel {
    /// Which detector won.
    pub kind: BackgroundKind,
    /// Representative background RGBA (cluster/bin mean; `[0;4]` for Otsu).
    pub rgba: [u8; 4],
    /// Agreement share (border-bin share, alpha share, or cluster share).
    pub consensus: f32,
}

/// Segmentation parameters — part of the vectorization cache key, so the
/// serialized form must stay field-stable and deterministic.
#[derive(Clone, Copy, Debug)]
pub struct SegParams {
    /// Minimum border-bin share to accept border consensus.
    pub border_share_min: f32,
    /// Ink decision threshold on ΔE(CIE-Lab) against the background.
    pub delta_e_max: f32,
    /// Alpha level at or above which a pixel counts as ink (Alpha path).
    pub alpha_ink_min: u8,
    /// Clean-stage close passes: 1 → 3×3 close, 2 → 5×5 (weak JPEG).
    pub close_passes: u8,
}

impl Default for SegParams {
    fn default() -> Self {
        Self {
            border_share_min: 0.85,
            delta_e_max: 12.0,
            alpha_ink_min: 128,
            close_passes: 1,
        }
    }
}

impl SegParams {
    /// Deterministic serialization for the cache key's `seg_params` slot.
    #[must_use]
    pub fn to_cache_string(&self) -> String {
        format!(
            "bgs{:.2}de{:.1}a{}cp{}",
            self.border_share_min, self.delta_e_max, self.alpha_ink_min, self.close_passes
        )
    }
}

/// Detects the sheet background (§3.3-② detector ladder).
#[must_use]
pub fn detect_background(r: &SheetRaster, p: &SegParams) -> BackgroundModel {
    if let Some(m) = detect_alpha(r) {
        return m;
    }
    if let Some(m) = detect_border_consensus(r, p.border_share_min) {
        return m;
    }
    if let Some(m) = detect_kmeans_corners(r) {
        return m;
    }
    detect_otsu(r)
}

fn detect_alpha(r: &SheetRaster) -> Option<BackgroundModel> {
    let n = r.width() as u64 * r.height() as u64;
    let transparent = r.rgba().chunks_exact(4).filter(|px| px[3] == 0).count() as u64;
    let share = transparent as f32 / n as f32;
    (share >= 0.10).then(|| BackgroundModel {
        kind: BackgroundKind::Alpha,
        rgba: [0, 0, 0, 0],
        consensus: share,
    })
}

/// Border ring → 4-4-4 RGB bins; the modal bin wins when its share clears
/// `min_share`. The background colour is the *mean of the exact colours* in
/// the winning bin (not the bin centre) for tighter ΔE.
fn detect_border_consensus(r: &SheetRaster, min_share: f32) -> Option<BackgroundModel> {
    let (w, h) = (r.width(), r.height());
    if w < 2 || h < 2 {
        return None;
    }
    let mut bins = [0u32; 4096];
    let mut border = 0u64;
    let ring = |x: u32, y: u32, bins: &mut [u32; 4096]| {
        let px = r.pixel(x, y);
        let b = (u16::from(px[0] >> 4) << 8) | (u16::from(px[1] >> 4) << 4) | u16::from(px[2] >> 4);
        bins[b as usize] += 1;
    };
    for x in 0..w {
        ring(x, 0, &mut bins);
        ring(x, h - 1, &mut bins);
        border += 2;
    }
    for y in 1..h - 1 {
        ring(0, y, &mut bins);
        ring(w - 1, y, &mut bins);
        border += 2;
    }
    let (best, best_share) = bins
        .iter()
        .enumerate()
        .map(|(b, &c)| (b, c as f32 / border as f32))
        .max_by(|a, b| a.1.total_cmp(&b.1).then(b.0.cmp(&a.0)))
        .unwrap();
    if best_share < min_share {
        return None;
    }
    // Mean of the exact colours in the winning bin (border ring only).
    let (mut sums, mut n) = ([0u64; 4], 0u64);
    let mut collect = |x: u32, y: u32| {
        let px = r.pixel(x, y);
        let b = (u16::from(px[0] >> 4) << 8) | (u16::from(px[1] >> 4) << 4) | u16::from(px[2] >> 4);
        if b as usize == best {
            for (s, c) in sums.iter_mut().zip(px) {
                *s += u64::from(c);
            }
            n += 1;
        }
    };
    for x in 0..w {
        collect(x, 0);
        collect(x, h - 1);
    }
    for y in 1..h - 1 {
        collect(0, y);
        collect(w - 1, y);
    }
    Some(BackgroundModel {
        kind: BackgroundKind::BorderConsensus,
        rgba: [
            (sums[0] / n) as u8,
            (sums[1] / n) as u8,
            (sums[2] / n) as u8,
            (sums[3] / n) as u8,
        ],
        consensus: best_share,
    })
}

/// k=2 k-means over the border ring, seeded with the top-left corner colour
/// and the farthest other corner. The background cluster is the one owning
/// more border pixels; the interior is classified by nearest centre.
fn detect_kmeans_corners(r: &SheetRaster) -> Option<BackgroundModel> {
    let (w, h) = (r.width(), r.height());
    if w < 2 || h < 2 {
        return None;
    }
    let mut ring: Vec<[u8; 3]> = Vec::new();
    for x in 0..w {
        ring.push(rgb_of(r.pixel(x, 0)));
        ring.push(rgb_of(r.pixel(x, h - 1)));
    }
    for y in 1..h - 1 {
        ring.push(rgb_of(r.pixel(0, y)));
        ring.push(rgb_of(r.pixel(w - 1, y)));
    }
    let seed0 = rgb_of(r.pixel(0, 0));
    let seed1 = ring
        .iter()
        .copied()
        .max_by(|a, b| {
            dist2(*a, seed0)
                .total_cmp(&dist2(*b, seed0))
                .then(a.cmp(b))
        })
        .unwrap();
    let (mut c0, mut c1) = (seed0, seed1);
    for _ in 0..12 {
        let (mut s0, mut n0, mut s1, mut n1) = ([0u32; 3], 0u32, [0u32; 3], 0u32);
        for px in &ring {
            if dist2(*px, c0) <= dist2(*px, c1) {
                s0[0] += u32::from(px[0]);
                s0[1] += u32::from(px[1]);
                s0[2] += u32::from(px[2]);
                n0 += 1;
            } else {
                s1[0] += u32::from(px[0]);
                s1[1] += u32::from(px[1]);
                s1[2] += u32::from(px[2]);
                n1 += 1;
            }
        }
        if n0 > 0 {
            c0 = [
                (s0[0] / n0) as u8,
                (s0[1] / n0) as u8,
                (s0[2] / n0) as u8,
            ];
        }
        if n1 > 0 {
            c1 = [
                (s1[0] / n1) as u8,
                (s1[1] / n1) as u8,
                (s1[2] / n1) as u8,
            ];
        }
    }
    // Border ownership vote → background cluster.
    let mut votes = [0u32; 2];
    for px in &ring {
        if dist2(*px, c0) <= dist2(*px, c1) {
            votes[0] += 1;
        } else {
            votes[1] += 1;
        }
    }
    let bg0 = votes[0] >= votes[1];
    let (bg, share) = if bg0 {
        (c0, votes[0] as f32 / ring.len() as f32)
    } else {
        (c1, votes[1] as f32 / ring.len() as f32)
    };
    Some(BackgroundModel {
        kind: BackgroundKind::KMeans,
        rgba: [bg[0], bg[1], bg[2], 255],
        consensus: share,
    })
}

fn detect_otsu(r: &SheetRaster) -> BackgroundModel {
    let mut hist = [0u64; 256];
    for &v in r.luma() {
        hist[v.round().clamp(0.0, 255.0) as usize] += 1;
    }
    let total = r.luma().len() as f64;
    let ink_share = minority_share(&hist, total);
    BackgroundModel {
        kind: BackgroundKind::Otsu,
        rgba: [0, 0, 0, 0],
        consensus: 1.0 - ink_share,
    }
}

/// Ink share of the Otsu split (minority side).
fn minority_share(hist: &[u64; 256], total: f64) -> f32 {
    let (t, ink_is_dark) = otsu_threshold(hist, total);
    let side: u64 = if ink_is_dark {
        hist[..=usize::from(t)].iter().sum()
    } else {
        hist[usize::from(t) + 1..].iter().sum()
    };
    side as f32 / total as f32
}

/// Maximal between-class variance split of a 256-bin luma histogram.
/// Returns `(threshold, ink_is_dark)` where ink is the minority side
/// (background dominates sheets).
fn otsu_threshold(hist: &[u64; 256], total: f64) -> (u8, bool) {
    let mut sum_all = 0.0f64;
    for (v, &c) in hist.iter().enumerate() {
        sum_all += v as f64 * c as f64;
    }
    let (mut w_b, mut sum_b, mut best_var, mut best_t) = (0.0f64, 0.0f64, -1.0f64, 0u8);
    for (v, &c) in hist.iter().enumerate() {
        w_b += c as f64;
        if w_b == 0.0 {
            continue;
        }
        let w_f = total - w_b;
        if w_f == 0.0 {
            break;
        }
        sum_b += v as f64 * c as f64;
        let m_b = sum_b / w_b;
        let m_f = (sum_all - sum_b) / w_f;
        let var = w_b * w_f * (m_b - m_f) * (m_b - m_f);
        if var > best_var {
            best_var = var;
            best_t = v as u8;
        }
    }
    let dark = hist[..=usize::from(best_t)].iter().sum::<u64>();
    let ink_is_dark = dark * 2 <= total as u64;
    (best_t, ink_is_dark)
}

fn rgb_of(px: [u8; 4]) -> [u8; 3] {
    [px[0], px[1], px[2]]
}

fn dist2(a: [u8; 3], b: [u8; 3]) -> f32 {
    let d = |x: u8, y: u8| f32::from(i16::from(x) - i16::from(y));
    d(a[0], b[0]) * d(a[0], b[0]) + d(a[1], b[1]) * d(a[1], b[1]) + d(a[2], b[2]) * d(a[2], b[2])
}

/// Builds the foreground mask (§3.3-② decision): ΔE(CIE-Lab) > threshold
/// against the background colour, or the Alpha/Otsu rule.
#[must_use]
pub fn build_mask(r: &SheetRaster, bg: &BackgroundModel, p: &SegParams) -> ForegroundMask {
    let (w, h) = (r.width(), r.height());
    let mut mask = ForegroundMask::new(w, h);
    match bg.kind {
        BackgroundKind::Alpha => {
            for y in 0..h {
                for x in 0..w {
                    if r.pixel(x, y)[3] >= p.alpha_ink_min {
                        mask.set(x, y, true);
                    }
                }
            }
        }
        BackgroundKind::BorderConsensus | BackgroundKind::KMeans => {
            let bg_lab = srgb_to_lab(bg.rgba[0], bg.rgba[1], bg.rgba[2]);
            // 256-entry LUT per channel for the sRGB transfer function.
            let lut = srgb_linear_lut();
            for y in 0..h {
                for x in 0..w {
                    let px = r.pixel(x, y);
                    if px[3] < p.alpha_ink_min {
                        continue; // fully transparent pixels are never ink
                    }
                    let lab = lab_from_lut(px[0], px[1], px[2], &lut);
                    if delta_e76(lab, bg_lab) > p.delta_e_max {
                        mask.set(x, y, true);
                    }
                }
            }
        }
        BackgroundKind::Otsu => {
            // Otsu on luma, ink = minority side of the border.
            let mut hist = [0u64; 256];
            for &v in r.luma() {
                hist[v.round().clamp(0.0, 255.0) as usize] += 1;
            }
            let total = r.luma().len() as f64;
            let (best_t, ink_is_dark) = otsu_threshold(&hist, total);
            for y in 0..h {
                for (x, &v) in r.luma_row(y).iter().enumerate() {
                    let v = v.round().clamp(0.0, 255.0) as u8;
                    let ink = if ink_is_dark {
                        v <= best_t
                    } else {
                        v > best_t
                    };
                    if ink {
                        mask.set(x as u32, y, true);
                    }
                }
            }
        }
    }
    mask
}

/// sRGB → CIE-Lab (D65). Straight per-call conversion; the mask builder
/// applies the transfer-function LUT on top for speed.
#[must_use]
pub fn srgb_to_lab(r: u8, g: u8, b: u8) -> [f32; 3] {
    let lut = srgb_linear_lut();
    lab_from_lut(r, g, b, &lut)
}

fn srgb_linear_lut() -> [f32; 256] {
    let mut lut = [0.0f32; 256];
    for (i, v) in lut.iter_mut().enumerate() {
        let c = i as f32 / 255.0;
        *v = if c > 0.04045 {
            ((c + 0.055) / 1.055).powf(2.4)
        } else {
            c / 12.92
        };
    }
    lut
}

fn lab_from_lut(r: u8, g: u8, b: u8, lut: &[f32; 256]) -> [f32; 3] {
    let (rl, gl, bl) = (lut[r as usize], lut[g as usize], lut[b as usize]);
    let x = (rl * 0.412_456_4 + gl * 0.357_576_1 + bl * 0.180_437_5) / 0.950_47;
    let y = rl * 0.212_672_9 + gl * 0.715_152_2 + bl * 0.072_175_0;
    let z = (rl * 0.019_333_9 + gl * 0.119_192_0 + bl * 0.950_304_1) / 1.088_83;
    let f = |t: f32| {
        if t > 216.0 / 24_389.0 {
            t.cbrt()
        } else {
            (24_389.0 / 27.0 * t + 16.0) / 116.0
        }
    };
    let (fx, fy, fz) = (f(x), f(y), f(z));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// CIE76 colour difference (euclidean Lab distance).
#[must_use]
pub fn delta_e76(a: [f32; 3], b: [f32; 3]) -> f32 {
    let d = a[0] - b[0];
    let mut acc = d * d;
    let d = a[1] - b[1];
    acc += d * d;
    let d = a[2] - b[2];
    acc += d * d;
    acc.sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Solid background `bg` with a filled `rect` in `fg`.
    #[allow(clippy::too_many_arguments)]
    fn sheet_with_rect(
        w: u32,
        h: u32,
        bg: [u8; 4],
        fg: [u8; 4],
        x0: u32,
        y0: u32,
        rw: u32,
        rh: u32,
    ) -> SheetRaster {
        let mut rgba = Vec::with_capacity(4 * w as usize * h as usize);
        for y in 0..h {
            for x in 0..w {
                let px = if x >= x0 && x < x0 + rw && y >= y0 && y < y0 + rh {
                    fg
                } else {
                    bg
                };
                rgba.extend_from_slice(&px);
            }
        }
        SheetRaster::from_rgba(w, h, rgba)
    }

    #[test]
    fn lab_reference_values_d65() {
        let white = srgb_to_lab(255, 255, 255);
        assert!((white[0] - 100.0).abs() < 0.02, "L* white = {}", white[0]);
        assert!(white[1].abs() < 0.02 && white[2].abs() < 0.02);
        let black = srgb_to_lab(0, 0, 0);
        assert!(black[0].abs() < 0.02);
        let red = srgb_to_lab(255, 0, 0);
        assert!((red[0] - 53.24).abs() < 0.1, "L* red = {}", red[0]);
        assert!((red[1] - 80.09).abs() < 0.2, "a* red = {}", red[1]);
        assert!((red[2] - 67.20).abs() < 0.2, "b* red = {}", red[2]);
    }

    #[test]
    fn delta_e_is_symmetric_and_zero_on_equal() {
        let a = srgb_to_lab(200, 30, 90);
        assert!(delta_e76(a, a) < 1e-6);
        let b = srgb_to_lab(30, 200, 90);
        assert!((delta_e76(a, b) - delta_e76(b, a)).abs() < 1e-6);
    }

    #[test]
    fn white_sheet_black_square_uses_border_consensus() {
        let r = sheet_with_rect(64, 64, [255, 255, 255, 255], [10, 10, 10, 255], 20, 20, 20, 20);
        let bg = detect_background(&r, &SegParams::default());
        assert_eq!(bg.kind, BackgroundKind::BorderConsensus);
        assert!(bg.consensus >= 0.85);
        assert_eq!(bg.rgba[0], 255);
        let mask = build_mask(&r, &bg, &SegParams::default());
        assert_eq!(mask.ink_count(), 20 * 20);
        assert!(mask.get(20, 20) && mask.get(39, 39));
        assert!(!mask.get(0, 0) && !mask.get(19, 20));
    }

    #[test]
    fn jpeg_style_noise_still_reaches_consensus_via_delta_e() {
        // Background ~250 with ±6 RGB noise (ΔE ≪ 12), icon at 60 (ΔE ≫ 12).
        // Range stays within 244..=255 so no u8 wrap can plant black px.
        let (w, h) = (96, 96);
        let mut rgba = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let n = ((x as i32 * 31 + y as i32 * 17) % 12) - 6;
                let px = if (20..40).contains(&x) && (20..40).contains(&y) {
                    [60, 60, 60, 255]
                } else {
                    [(250 + n) as u8, (250 + n) as u8, (250 + n) as u8, 255]
                };
                rgba.extend_from_slice(&px);
            }
        }
        let r = SheetRaster::from_rgba(w, h, rgba);
        let bg = detect_background(&r, &SegParams::default());
        assert_eq!(bg.kind, BackgroundKind::BorderConsensus, "kind {:?}", bg.kind);
        let mask = build_mask(&r, &bg, &SegParams::default());
        let expected = 20 * 20;
        // Tolerance for AA-free but noisy edges must be small — ΔE is the
        // speckle killer.
        let got = mask.ink_count();
        assert!(
            (got as i64 - expected as i64).abs() <= 40,
            "ink {got} vs {expected}"
        );
    }

    #[test]
    fn transparent_background_takes_alpha_path() {
        let r = sheet_with_rect(32, 32, [0, 0, 0, 0], [200, 40, 40, 255], 8, 8, 16, 16);
        let bg = detect_background(&r, &SegParams::default());
        assert_eq!(bg.kind, BackgroundKind::Alpha);
        let mask = build_mask(&r, &bg, &SegParams::default());
        assert_eq!(mask.ink_count(), 16 * 16);
    }

    #[test]
    fn weak_consensus_falls_back_to_corner_kmeans() {
        // 50/50 two-tone: no border majority → corner-seeded k-means.
        let (w, h) = (64, 64);
        let mut rgba = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let px = if (x + y).is_multiple_of(2) {
                    [255, 255, 255, 255]
                } else {
                    [0, 0, 0, 255]
                };
                rgba.extend_from_slice(&px);
            }
        }
        let r = SheetRaster::from_rgba(w, h, rgba);
        let bg = detect_background(&r, &SegParams::default());
        assert_eq!(bg.kind, BackgroundKind::KMeans);
    }

    #[test]
    fn cache_string_is_stable() {
        let p = SegParams::default();
        assert_eq!(p.to_cache_string(), "bgs0.85de12.0a128cp1");
    }
}
