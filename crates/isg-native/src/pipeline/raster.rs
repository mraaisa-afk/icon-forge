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
        let px_rows = rgba.as_chunks::<4>().0;
        let luma: Vec<f32> = px_rows
            .iter()
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

    /// Encodes [`Self::crop_rgba`] as a lossless PNG — the comparator's
    /// A-side, pixel-identical to what stage ⑧ scored.
    #[must_use]
    pub fn crop_png(&self, bbox: Bbox) -> Vec<u8> {
        use image::ImageEncoder;
        let rgba = self.crop_rgba(bbox);
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&rgba, bbox.w, bbox.h, image::ExtendedColorType::Rgba8)
            // In-memory sink: the encoder cannot fail on a Vec target.
            .expect("png encode into Vec is infallible");
        png
    }
}

impl SheetRaster {
    /// Box-average downscale so the longest side is at most `max_dim` (never
    /// upscales). Returns the output dimensions plus RGBA8 bytes.
    ///
    /// Every output pixel is the mean of the source pixels it covers, rounded
    /// half-up in integer arithmetic: same input bytes ⇒ same output bytes,
    /// which is what lets the overlay cache the encoded PNG.
    #[must_use]
    pub fn scaled_rgba(&self, max_dim: u32) -> (u32, u32, Vec<u8>) {
        let longest = self.width.max(self.height);
        if max_dim == 0 || longest <= max_dim {
            return (self.width, self.height, self.rgba.clone());
        }
        let scale = |dim: u32| -> u32 {
            (((u64::from(dim) * u64::from(max_dim)) / u64::from(longest)).max(1)) as u32
        };
        let (out_w, out_h) = (scale(self.width), scale(self.height));
        let mut out = vec![0u8; 4 * out_w as usize * out_h as usize];
        for oy in 0..out_h {
            let y0 = (u64::from(oy) * u64::from(self.height) / u64::from(out_h)) as u32;
            let y1 = (((u64::from(oy) + 1) * u64::from(self.height) / u64::from(out_h)) as u32)
                .max(y0 + 1)
                .min(self.height);
            for ox in 0..out_w {
                let x0 = (u64::from(ox) * u64::from(self.width) / u64::from(out_w)) as u32;
                let x1 = (((u64::from(ox) + 1) * u64::from(self.width) / u64::from(out_w)) as u32)
                    .max(x0 + 1)
                    .min(self.width);
                let mut sums = [0u64; 4];
                let mut count = 0u64;
                for y in y0..y1 {
                    let base = y as usize * self.width as usize * 4;
                    for x in x0..x1 {
                        let px = &self.rgba[base + x as usize * 4..base + x as usize * 4 + 4];
                        sums[0] += u64::from(px[0]);
                        sums[1] += u64::from(px[1]);
                        sums[2] += u64::from(px[2]);
                        sums[3] += u64::from(px[3]);
                        count += 1;
                    }
                }
                let o = (oy as usize * out_w as usize + ox as usize) * 4;
                // Round half-up: (2·sum + count) / (2·count).
                let mean = [
                    ((2 * sums[0] + count) / (2 * count)) as u8,
                    ((2 * sums[1] + count) / (2 * count)) as u8,
                    ((2 * sums[2] + count) / (2 * count)) as u8,
                    ((2 * sums[3] + count) / (2 * count)) as u8,
                ];
                out[o..o + 4].copy_from_slice(&mean);
            }
        }
        (out_w, out_h, out)
    }

    /// [`Self::scaled_rgba`] encoded as a lossless PNG — the overlay's
    /// backdrop (the sheet the groups were measured on).
    #[must_use]
    pub fn preview_png(&self, max_dim: u32) -> (u32, u32, Vec<u8>) {
        use image::ImageEncoder;
        let (w, h, rgba) = self.scaled_rgba(max_dim);
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&rgba, w, h, image::ExtendedColorType::Rgba8)
            .expect("png encode into Vec is infallible");
        (w, h, png)
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
    fn scaled_rgba_box_averages_and_never_upscales() {
        // 4×4: left half black (opaque), right half white (opaque).
        let mut rgba = vec![255u8; 4 * 4 * 4];
        for y in 0..4usize {
            for x in 0..2usize {
                let i = (y * 4 + x) * 4;
                rgba[i] = 0;
                rgba[i + 1] = 0;
                rgba[i + 2] = 0;
            }
        }
        let r = SheetRaster::from_rgba(4, 4, rgba);
        let (w, h, out) = r.scaled_rgba(2);
        assert_eq!((w, h), (2, 2));
        assert_eq!(out.len(), 4 * 4);
        // Each output pixel covers a 2×2 source block ⇒ mean 0 on the left,
        // 255 on the right; alpha stays 255 everywhere.
        assert_eq!(&out[0..4], &[0, 0, 0, 255]);
        assert_eq!(&out[4..8], &[255, 255, 255, 255]);
        assert_eq!(&out[12..16], &[255, 255, 255, 255]);
        // Identical input ⇒ identical output (the overlay cache relies on it).
        assert_eq!(r.scaled_rgba(2).2, out);
        // Never upscales.
        let (w, h, same) = r.scaled_rgba(64);
        assert_eq!((w, h), (4, 4));
        assert_eq!(same, r.rgba());
        // A long thin sheet keeps its aspect ratio.
        let wide = SheetRaster::from_rgba(8, 2, vec![128; 8 * 2 * 4]);
        assert_eq!(wide.scaled_rgba(4), (4, 1, vec![128; 16]));
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
