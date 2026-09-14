//! Stage 1 (spike): border-median background thresholding.
//!
//! The median of the outermost pixel ring is a robust background estimate
//! (corpus icons stay ≥ 2 px off the border, so icon pixels never dominate
//! the ring). Foreground = |luma − bg_median| ≥ threshold.
//!
//! Phase 2 replaces this with CIE-Lab ΔE background detection — the
//! [`ForegroundMasker`] trait is the stable seam.

use isg_core::{ForegroundMask, ForegroundMasker, RasterView};

/// Border-median foreground masker.
#[derive(Clone, Copy, Debug)]
pub struct BorderMedianMasker {
    /// Minimum absolute luma deviation from the border median to count as
    /// foreground (0–255 scale).
    pub threshold: f32,
}

impl Default for BorderMedianMasker {
    fn default() -> Self {
        Self { threshold: 32.0 }
    }
}

/// Median of the outermost ring luma values.
///
/// Note: panics on a zero-height raster — unreachable through decoders, kept
/// simple for the spike.
#[must_use]
pub fn border_median(raster: &dyn RasterView) -> f32 {
    let w = raster.width() as usize;
    let h = raster.height() as usize;
    let mut ring: Vec<f32> = Vec::with_capacity(2 * w + 2 * h.saturating_sub(2));
    for x in 0..w {
        ring.push(raster.luma_row(0)[x]);
        ring.push(raster.luma_row((h - 1) as u32)[x]);
    }
    for y in 1..h - 1 {
        let row = raster.luma_row(y as u32);
        ring.push(row[0]);
        ring.push(row[w - 1]);
    }
    ring.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    ring[ring.len() / 2]
}

impl ForegroundMasker for BorderMedianMasker {
    fn foreground(&self, raster: &dyn RasterView) -> ForegroundMask {
        let w = raster.width();
        let h = raster.height();
        let bg = border_median(raster);
        let mut out = ForegroundMask::new(w, h);
        for y in 0..h {
            for (x, &v) in raster.luma_row(y).iter().enumerate() {
                if (v - bg).abs() >= self.threshold {
                    out.set(x as u32, y, true);
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use isg_core::RasterView;

    struct M {
        w: u32,
        h: u32,
        luma: Vec<f32>,
    }
    impl RasterView for M {
        fn width(&self) -> u32 {
            self.w
        }
        fn height(&self) -> u32 {
            self.h
        }
        fn luma_row(&self, y: u32) -> &[f32] {
            &self.luma[y as usize * self.w as usize..(y + 1) as usize * self.w as usize]
        }
    }

    #[test]
    fn masks_dark_icon_on_light_sheet() {
        let mut luma = vec![255.0f32; 16 * 16];
        // 4x4 black icon at (6,6)
        for y in 6..10 {
            for x in 6..10 {
                luma[y * 16 + x] = 0.0;
            }
        }
        let r = M { w: 16, h: 16, luma };
        let m = BorderMedianMasker::default().foreground(&r);
        assert_eq!(m.ink_count(), 16, "icon pixels only");
        assert!(m.get(6, 6));
        assert!(!m.get(0, 0));
        assert!(!m.get(5, 6));
    }

    #[test]
    fn masks_light_icon_on_dark_sheet() {
        // Polarity independence: bg 30, icon 220 — the same |Δ| rule must fire.
        let mut luma = vec![30.0f32; 16 * 16];
        for y in 6..10 {
            for x in 6..10 {
                luma[y * 16 + x] = 220.0;
            }
        }
        let r = M { w: 16, h: 16, luma };
        let m = BorderMedianMasker::default().foreground(&r);
        assert_eq!(m.ink_count(), 16, "icon pixels only (light-on-dark)");
    }
}
