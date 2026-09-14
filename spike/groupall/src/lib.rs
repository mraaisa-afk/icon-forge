//! Phase 0 spike: raster sheet → foreground mask → CCL icon groups → per-group
//! SVG (vtracer). Throwaway code — the *traits* it exercises live in
//! [`isg_core`] and are frozen as of Phase 1 (see the crate docs there).
//!
//! Pipeline (mirrors the Phase 2 stage names):
//!
//! ```text
//! decode ─▶ mask (border-median) ─▶ CCL group ─▶ trace (vtracer, rayon) ─▶ SVGs
//! ```

pub mod raster;
pub mod mask;
pub mod group;
pub mod trace;
pub mod pipeline;
pub mod report;
