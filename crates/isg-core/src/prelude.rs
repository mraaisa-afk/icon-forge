//! The frozen public surface of `isg-core` (Phase 1).
//!
//! Import this prelude to use the pipeline seams. Items here are frozen —
//! see the crate docs. `Leveler` and `ReviewScorer` join in phases 5/6.
pub use crate::{
    Bbox, ForegroundMask, ForegroundMasker, GroupAllOutput, GroupingStrategy, IconGroup, RasterView,
    RleRun, SheetPipeline, TraceError, TracePreset, VectorTracer,
};
