//! "Group All" orchestration: mask → CCL → per-group trace (rayon).
//!
//! vtracer's `Pipeline` is not `Send` (its stages are plain `dyn` objects), so
//! each rayon task builds its own — construction is a handful of allocations
//! against milliseconds of segmentation work.

use isg_core::{
    ForegroundMasker, GroupAllOutput, GroupingStrategy, SheetPipeline, TraceError, TracePreset,
    VectorTracer,
};
use rayon::prelude::*;

use crate::group::CclGrouper;
use crate::mask::{border_median, BorderMedianMasker};
use crate::trace::VtracerTracer;

/// The spike's end-to-end sheet pipeline.
pub struct GroupAllPipeline {
    pub masker: BorderMedianMasker,
    pub grouper: CclGrouper,
    pub preset: TracePreset,
}

impl Default for GroupAllPipeline {
    fn default() -> Self {
        Self {
            masker: BorderMedianMasker::default(),
            grouper: CclGrouper::default(),
            preset: TracePreset::Draft,
        }
    }
}

impl SheetPipeline for GroupAllPipeline {
    fn group_all(&self, raster: &dyn isg_core::RasterView) -> GroupAllOutput {
        let mask = self.masker.foreground(raster);
        let groups = self.grouper.group_all(raster, &mask);
        // Background estimated once per sheet; crops binarize relative to it
        // (no hardcoded 128 — light-on-dark and mid-tone inks survive).
        let bg = border_median(raster);
        let preset = self.preset;
        let svgs: Vec<_> = groups
            .par_iter()
            .map(|g| {
                let tracer = VtracerTracer::new(preset, bg)
                    .map_err(|e| TraceError::TraceFailed(format!("tracer: {e}")))?;
                tracer.trace(raster, &g.bbox, preset)
            })
            .collect();
        GroupAllOutput { groups, svgs }
    }
}
