//! Provisional prelude: the Phase 0 public surface of `isg-core`.
//!
//! These re-exports are **spike-scoped and not frozen** — see the crate docs.
//! The freeze happens when the mask/run types are reworked to the
//! architecture's bit-packed / RLE representation.
pub use crate::{
    Bbox, ForegroundMasker, GroupAllOutput, GroupingStrategy, IconGroup, RasterView, SheetPipeline,
    TraceError, TracePreset, VectorTracer,
};
