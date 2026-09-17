//! Stage 3 (spike): vtracer-backed per-group SVG tracing.
//!
//! The crop is binarized *relative to the sheet's background luma* — the same
//! decision the masker made — so foreground polarity (dark-on-light or
//! light-on-dark) and mid-tone inks both survive. vtracer runs in binary mode;
//! the pipeline object is built per crop task (it is not `Send`).

use isg_core::{Bbox, RasterView, TraceError, TracePreset, VectorTracer};

/// vtracer adapter for the spike (binary/draft configuration).
pub struct VtracerTracer {
    /// Background luma of the *sheet* (border median, computed once).
    bg: f32,
    /// Minimum |luma − bg| for a crop pixel to count as foreground ink.
    fg_deviation: f32,
    pipeline: vtracer::Pipeline,
}

impl VtracerTracer {
    /// Builds the tracer. `bg` is the sheet's border-median background luma.
    pub fn new(preset: TracePreset, bg: f32) -> Result<Self, TraceError> {
        let (mode, speckle, corner) = match preset {
            TracePreset::Draft => (vtracer::FitMode::Polygon, 4, 60),
            TracePreset::Balanced => (vtracer::FitMode::Spline, 4, 60),
            TracePreset::Pixel => (vtracer::FitMode::Pixel, 1, 30),
            // The spike only exercises a few presets; the rest map to
            // Balanced-equivalent options until Phase 2 implements the table.
            _ => (vtracer::FitMode::Spline, 4, 60),
        };
        let mut cfg = vtracer::Config::from_preset(vtracer::Preset::Bw);
        cfg.clustering = vtracer::Clustering::Binary;
        cfg.mode = mode;
        cfg.filter_speckle = speckle;
        cfg.corner_threshold = corner;
        cfg.binary_threshold = 128;
        cfg.optimize = 1;
        let pipeline = cfg
            .build()
            .map_err(|e| TraceError::TraceFailed(format!("pipeline build: {e}")))?;
        Ok(Self {
            bg,
            fg_deviation: 32.0,
            pipeline,
        })
    }

    /// Crop the `bbox` region into a binary RGBA image: foreground ink black
    /// on white, polarity-independent (works for light-on-dark sheets too).
    fn crop_rgba(&self, raster: &dyn RasterView, bbox: &Bbox) -> Vec<u8> {
        let w = bbox.w as usize;
        let h = bbox.h as usize;
        let mut out = Vec::with_capacity(w * h * 4);
        for y in 0..bbox.h {
            let row = raster.luma_row(bbox.y + y);
            for x in 0..bbox.w {
                let v = row[(bbox.x + x) as usize];
                let c = if (v - self.bg).abs() >= self.fg_deviation {
                    0u8
                } else {
                    255u8
                };
                out.push(c);
                out.push(c);
                out.push(c);
                out.push(255);
            }
        }
        out
    }
}

impl VectorTracer for VtracerTracer {
    fn trace(
        &self,
        raster: &dyn RasterView,
        bbox: &Bbox,
        _preset: TracePreset,
    ) -> Result<String, TraceError> {
        // The preset is honoured at construction time (the spike builds one
        // tracer per task); per-call changes would rebuild in Phase 2.
        if bbox.is_empty() {
            return Err(TraceError::EmptyBbox);
        }
        let pixels = self.crop_rgba(raster, bbox);
        let img = vtracer::ColorImage {
            pixels,
            width: bbox.w as usize,
            height: bbox.h as usize,
        };
        self.pipeline
            .to_svg(&img)
            .map_err(|e| TraceError::TraceFailed(e.to_string()))
    }
}
