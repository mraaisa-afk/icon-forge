//! Decoded sheet raster shared by the §3.3 stages.
//!
//! [`SheetRaster`] owns the RGBA8 pixels (colour stages ② and ④) plus a
//! materialized luma plane that implements the frozen
//! [`isg_core::RasterView`] seam (masking ② and grouping). Luma follows the
//! same Rec.601 weights the Phase 0 spike used, so thresholds stay comparable.

use isg_core::{Bbox, RasterView};

/// RGBA8 sheet plus its luma plane (row-major, 0.0–255.0, Rec.601).
#[derive(Clone, Debug)]
pub struct SheetRaster {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    luma: Vec<f32>,
}

impl SheetRaster {
    /// Builds a raster from raw RGBA8 bytes, computing the luma plane.
    ///
    /// # Panics
    /// Panics when `rgba.len() != 4 * width * height`.
    #[must_use]
    pub fn from_rgba(width: u32, height: u32, rgba: Vec<u8>) -> Self {
        assert_eq!(
            rgba.len(),
            4 * width as usize * height as usize,
            "RGBA8 byte count mismatch"
        );
        let luma: Vec<f32> = rgba
            .chunks_exact(4)
            .map(|px| {
                0.299 * f32::from(px[0]) + 0.587 * f32::from(px[1]) + 0.114 * f32::from(px[2])
            })
            .collect();
        Self {
            width,
            height,
            rgba,
            luma,
        }
    }

    /// Width in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Height in pixels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// RGBA8 bytes (length `4 * width * height`, row-major).
    #[must_use]
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }

    /// Luma plane (length `width * height`, row-major, 0.0–255.0).
    #[must_use]
    pub fn luma(&self) -> &[f32] {
        &self.luma
    }

    /// RGBA value of pixel `(x, y)`.
    ///
    /// # Panics
    /// Panics when `(x, y)` is outside the raster.
    #[must_use]
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = (y as usize * self.width as usize + x as usize) * 4;
        [
            self.rgba[i],
            self.rgba[i + 1],
            self.rgba[i + 2],
            self.rgba[i + 3],
        ]
    }

    /// Crops an RGBA8 region (`(w * h * 4)` bytes, row-major) — the per-icon
    /// crop fed to colour tracing and scoring.
    #[must_use]
    pub fn crop_rgba(&self, bbox: Bbox) -> Vec<u8> {
        let mut out = Vec::with_capacity(4 * bbox.w as usize * bbox.h as usize);
        for y in bbox.y..bbox.y + bbox.h {
            let row = (y as usize * self.width as usize + bbox.x as usize) * 4;
            let span = 4 * bbox.w as usize;
            out.extend_from_slice(&self.rgba[row..row + span]);
        }
        out
    }
}

impl RasterView for SheetRaster {
    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn luma_row(&self, y: u32) -> &[f32] {
        let w = self.width as usize;
        &self.luma[y as usize * w..(y as usize + 1) * w]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2×2 raster: red, green / blue, transparent-white.
    fn sample() -> SheetRaster {
        SheetRaster::from_rgba(
            2,
            2,
            vec![
                255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 240, 240, 240, 0,
            ],
        )
    }

    #[test]
    fn luma_uses_rec601_weights() {
        let r = sample();
        assert_eq!(r.luma_row(0).len(), 2);
        let red = 0.299 * 255.0;
        assert!((r.luma_row(0)[0] - red).abs() < 0.01, "red luma {red}");
        let gray = 0.299 * 240.0 + 0.587 * 240.0 + 0.114 * 240.0;
        assert!((r.luma_row(1)[1] - gray).abs() < 0.01);
    }

    #[test]
    fn crop_rgba_extracts_exact_region() {
        let r = sample();
        let bbox = Bbox::new(1, 0, 1, 2).unwrap();
        assert_eq!(r.crop_rgba(bbox), vec![0, 255, 0, 255, 240, 240, 240, 0]);
    }

    #[test]
    fn pixel_reads_rgba() {
        let r = sample();
        assert_eq!(r.pixel(0, 0), [255, 0, 0, 255]);
        assert_eq!(r.pixel(1, 1)[3], 0);
    }
}
