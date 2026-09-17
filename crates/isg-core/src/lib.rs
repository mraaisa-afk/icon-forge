//! # isg-core — Icon Forge core
//!
//! Shared types and traits for the Icon Forge vectorization pipeline.
//!
//! ## Keystone architecture decision
//!
//! `isg-core` is compiled **twice**:
//!
//! * **native** — linked into the Tauri binary; batch work (decoding, masking,
//!   CCL, tracing) runs here under `rayon`,
//! * **wasm32-unknown-unknown** — loaded into the webview; interactive geometry
//!   (hit-testing, snapping, booleans, auto-leveling) runs in-browser at
//!   sub-millisecond latency.
//!
//! To keep that contract, `isg-core` is dependency-free (std only), contains
//! no `rayon` / platform-specific code, and its `Send + Sync` bound surface is
//! stable. CI enforces the contract:
//!
//! ```text
//! cargo check --target wasm32-unknown-unknown -p isg-core
//! ```
//!
//! ## Frozen surface (Phase 1)
//!
//! The items re-exported by [`prelude`] are **frozen as of Phase 1**:
//! [`Bbox`], [`IconGroup`], [`TracePreset`], [`TraceError`], [`RasterView`],
//! [`ForegroundMask`] + [`RleRun`], [`ForegroundMasker`],
//! [`GroupingStrategy`], [`VectorTracer`], [`GroupAllOutput`] and
//! [`SheetPipeline`]. The Phase 0 spike ran on a provisional `Vec<bool>`
//! mask; per the Phase 0 review decision (F10) the mask representation was
//! first reworked to the architecture-mandated bit-packed 1-bpp storage with
//! RLE row extraction (see [`mask`]) — and only then frozen.
//!
//! Freezing means: breaking changes to these items require an explicit,
//! documented decision (the equivalent of a golden-file update) — downstream
//! crates (`isg-native`, `src-tauri`, `isg-wasm`) and the WASM keystone check
//! compile against them on every commit.
//!
//! The `Leveler` and `ReviewScorer` traits (ARCHITECTURE.md §7) land and
//! freeze in their own phases (5 and 6).

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod editor;
pub mod mask;
pub mod prelude;

pub use crate::mask::{ForegroundMask, RleRun};

/// A tight axis-aligned bounding box in raster (pixel) coordinates.
///
/// `x`/`y` is the top-left corner (inclusive); `w`/`h` are widths in pixels
/// (a 1×1 box covers exactly its top-left pixel). Coordinates are sheet
/// coordinates (top-left origin, y down), matching both raster decoders and
/// SVG viewBox semantics.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Bbox {
    /// Left edge, inclusive.
    pub x: u32,
    /// Top edge, inclusive.
    pub y: u32,
    /// Width in pixels (≥ 1 for non-empty boxes).
    pub w: u32,
    /// Height in pixels (≥ 1 for non-empty boxes).
    pub h: u32,
}

impl Bbox {
    /// The empty box (`w == 0 || h == 0`).
    pub const fn empty() -> Self {
        Self {
            x: 0,
            y: 0,
            w: 0,
            h: 0,
        }
    }

    /// Creates a box, returning `None` when the extent is empty.
    #[must_use]
    pub const fn new(x: u32, y: u32, w: u32, h: u32) -> Option<Self> {
        if w == 0 || h == 0 {
            return None;
        }
        Some(Self { x, y, w, h })
    }

    /// Creates a box unconditionally (panics on empty extents in debug builds).
    #[must_use]
    pub fn from_parts(x: u32, y: u32, w: u32, h: u32) -> Self {
        debug_assert!(
            w > 0 && h > 0,
            "Bbox::from_parts requires non-empty extents"
        );
        Self { x, y, w, h }
    }

    /// True when the box covers no pixels.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }

    /// Number of covered pixels (`w * h` in u64 to avoid 32-bit overflow).
    #[must_use]
    pub const fn area(&self) -> u64 {
        self.w as u64 * self.h as u64
    }

    /// True when the point `(px, py)` lies inside the box (inclusive).
    #[must_use]
    pub const fn contains_point(&self, px: u32, py: u32) -> bool {
        !self.is_empty()
            && px >= self.x
            && py >= self.y
            && px < self.x + self.w
            && py < self.y + self.h
    }

    /// True when the boxes share at least one pixel.
    #[must_use]
    pub const fn intersects(&self, other: &Self) -> bool {
        !self.is_empty()
            && !other.is_empty()
            && self.x < other.x + other.w
            && other.x < self.x + self.w
            && self.y < other.y + other.h
            && other.y < self.y + self.h
    }

    /// Smallest box covering both. Identity element is [`Bbox::empty`].
    #[must_use]
    pub fn union(&self, other: &Self) -> Self {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let x2 = self
            .x
            .saturating_add(self.w)
            .max(other.x.saturating_add(other.w));
        let y2 = self
            .y
            .saturating_add(self.h)
            .max(other.y.saturating_add(other.h));
        Self {
            x,
            y,
            w: x2 - x,
            h: y2 - y,
        }
    }

    /// Box grown by `n` pixels on every side, clamped to `canvas`.
    #[must_use]
    pub fn expand(&self, n: u32, canvas: &Self) -> Self {
        if self.is_empty() {
            return *self;
        }
        let x = self.x.saturating_sub(n).max(canvas.x);
        let y = self.y.saturating_sub(n).max(canvas.y);
        let x2 = self
            .x
            .saturating_add(self.w)
            .saturating_add(n)
            .min(canvas.x + canvas.w);
        let y2 = self
            .y
            .saturating_add(self.h)
            .saturating_add(n)
            .min(canvas.y + canvas.h);
        Self::new(x, y, x2.saturating_sub(x), y2.saturating_sub(y)).unwrap_or(*self)
    }

    /// Geometric center in pixel coordinates.
    #[must_use]
    pub fn center(&self) -> (f32, f32) {
        (
            self.x as f32 + self.w as f32 / 2.0,
            self.y as f32 + self.h as f32 / 2.0,
        )
    }
}

/// One icon group produced by segmentation of a sheet.
///
/// `bbox` is the *tight* bounding box of the group's foreground pixels;
/// `area` is the foreground pixel count; `origin` is the top-left-scanned
/// member pixel. Together they give a total, deterministic order
/// (`bbox.y`, `bbox.x`, `origin`) that downstream stages (and tests) rely on
/// for byte-determinism.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IconGroup {
    /// Tight bounding box in sheet coordinates.
    pub bbox: Bbox,
    /// Foreground pixel count.
    pub area: u32,
    /// First (top-left-most) member pixel in raster scan order.
    pub origin: (u32, u32),
}

/// Trace quality presets (Phase 2 implements the full mapping to the
/// vectorizer option set; Phase 0 exercises the `Draft` end-to-end).
///
/// Seven presets from fastest/sketchiest to most faithful:
///
/// | preset | intent |
/// |---|---|
/// | `Draft` | fastest trace, coarse corner tolerance, heavy speckle filter |
/// | `Wireframe` | outlines only (no fills) |
/// | `Lineart` | thin, clean strokes; preserves 1-px features |
/// | `Balanced` | default for review workflows |
/// | `Detailed` | tighter fit, keeps small features |
/// | `HighFidelity` | near-lossless geometry, splines everywhere |
/// | `Pixel` | pixel-accurate staircase fit (debugging reference) |
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TracePreset {
    /// Fastest trace, coarse corners, heavy speckle filter.
    Draft,
    /// Outlines only (no fills).
    Wireframe,
    /// Thin, clean strokes; preserves 1-px features.
    Lineart,
    /// Default for review workflows.
    Balanced,
    /// Tighter fit, keeps small features.
    Detailed,
    /// Near-lossless geometry.
    HighFidelity,
    /// Pixel-accurate staircase fit (debugging reference).
    Pixel,
}

impl TracePreset {
    /// Stable 1-based index used for cache keys and UI ordering.
    #[must_use]
    pub const fn ordinal(self) -> u8 {
        match self {
            TracePreset::Draft => 1,
            TracePreset::Wireframe => 2,
            TracePreset::Lineart => 3,
            TracePreset::Balanced => 4,
            TracePreset::Detailed => 5,
            TracePreset::HighFidelity => 6,
            TracePreset::Pixel => 7,
        }
    }
}

/// Errors surfaced by the vectorization or tracing stage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TraceError {
    /// The requested region contained no pixels.
    EmptyBbox,
    /// The vectorizer failed to produce output.
    TraceFailed(String),
    /// Produced SVG failed re-parsing (Phase 2 validates with `usvg`; the
    /// trait carries the variant now so adapters can already report it).
    UnparseableSvg,
}

impl std::fmt::Display for TraceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TraceError::EmptyBbox => write!(f, "trace target is empty"),
            TraceError::TraceFailed(msg) => write!(f, "trace failed: {msg}"),
            TraceError::UnparseableSvg => write!(f, "produced SVG failed to re-parse"),
        }
    }
}

impl std::error::Error for TraceError {}

/// A read-only, row-major luma view of a raster image.
///
/// Implementations decode the source format once and expose 8-bit luma
/// (0.0–255.0) as `f32`. Row `y` is a slice of length `width()`.
///
/// Luma (not full RGBA) is the Phase 0 contract: segmentation and BW tracing
/// are luma-domain operations, and it keeps hot loops cache-friendly. Colour
/// work (quantization, ΔE background detection) arrives in Phase 2 and will
/// extend — not replace — this view.
pub trait RasterView: Send + Sync {
    /// Width in pixels.
    fn width(&self) -> u32;
    /// Height in pixels.
    fn height(&self) -> u32;
    /// Luma values for row `y`, length `width()`.
    fn luma_row(&self, y: u32) -> &[f32];
}

/// Stage 1 of the pipeline: raster → foreground mask.
///
/// Phase 0 used border-median thresholding; Phase 2 replaces this with
/// CIE-Lab ΔE background detection — the trait is the stable seam.
///
/// Returns a bit-packed [`ForegroundMask`] (see [`crate::mask`]).
pub trait ForegroundMasker: Send + Sync {
    /// Computes the foreground mask for `raster`.
    fn foreground(&self, raster: &dyn RasterView) -> ForegroundMask;
}

/// Stage 2 of the pipeline: foreground mask → icon groups.
///
/// Implementations must be deterministic: equal inputs produce equal output
/// in equal order (sorted by `bbox.y`, `bbox.x`, then `origin`).
pub trait GroupingStrategy: Send + Sync {
    /// Groups every foreground pixel of `mask` into icon groups.
    ///
    /// The mask is the shared bit-packed representation; production
    /// implementations (Phase 3) consume [`ForegroundMask::runs`] directly.
    fn group_all(&self, raster: &dyn RasterView, mask: &ForegroundMask) -> Vec<IconGroup>;
}

/// Stage 3 of the pipeline: one group's crop → standalone SVG.
///
/// Note: unlike the other stage traits, this one does not require
/// `Send + Sync` — backends may hold per-task state (e.g. the vtracer
/// pipeline object is not `Send`). Adapters that must be shared across
/// rayon workers should still be `Send + Sync`.
pub trait VectorTracer {
    /// Traces the region `bbox` of `raster` with `preset` into a standalone
    /// SVG string (viewBox in crop-local coordinates, origin at the crop's
    /// top-left).
    fn trace(
        &self,
        raster: &dyn RasterView,
        bbox: &Bbox,
        preset: TracePreset,
    ) -> Result<String, TraceError>;
}

/// Full "Group All" result for one sheet: groups in deterministic order with
/// the corresponding per-group trace output.
#[derive(Clone, Debug)]
pub struct GroupAllOutput {
    /// Detected icon groups, in deterministic order.
    pub groups: Vec<IconGroup>,
    /// Trace output per group; `svgs[i]` corresponds to `groups[i]`.
    pub svgs: Vec<Result<String, TraceError>>,
}

impl GroupAllOutput {
    /// Number of groups whose trace succeeded.
    #[must_use]
    pub fn trace_successes(&self) -> usize {
        self.svgs.iter().filter(|s| s.is_ok()).count()
    }

    /// True when every group traced successfully.
    #[must_use]
    pub fn all_traced(&self) -> bool {
        self.trace_successes() == self.groups.len()
    }
}

/// End-to-end sheet processor ("Group All"): mask → group → trace.
///
/// The Phase 0 spike implements this with the vtracer backend; Phase 2
/// replaces the internals with the full 8-stage pipeline behind the same
/// seam, so batch orchestration, caching and the review workspace never
/// depend on a particular stage implementation.
pub trait SheetPipeline: Send + Sync {
    /// Runs the complete group-all pipeline over one sheet.
    fn group_all(&self, raster: &dyn RasterView) -> GroupAllOutput;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bbox_basic() {
        assert!(Bbox::new(0, 0, 0, 5).is_none());
        assert!(Bbox::empty().is_empty());
        let a = Bbox::new(10, 20, 5, 7).unwrap();
        assert_eq!(a.area(), 35);
        assert!(a.contains_point(10, 20));
        assert!(a.contains_point(14, 26));
        assert!(!a.contains_point(15, 26));
        assert!(!a.contains_point(9, 26));
    }

    #[test]
    fn bbox_union_and_intersects() {
        let a = Bbox::new(0, 0, 10, 10).unwrap();
        let b = Bbox::new(5, 5, 10, 10).unwrap();
        assert!(a.intersects(&b));
        let u = a.union(&b);
        assert_eq!(u, Bbox::new(0, 0, 15, 15).unwrap());
        let c = Bbox::new(10, 10, 5, 5).unwrap();
        assert!(
            !a.intersects(&c),
            "corner touch is not intersection (pixel-based)"
        );
        assert_eq!(a.union(&Bbox::empty()), a);
    }

    #[test]
    fn bbox_expand_clamps_to_canvas() {
        let canvas = Bbox::new(0, 0, 100, 100).unwrap();
        let a = Bbox::new(0, 0, 10, 10).unwrap();
        let e = a.expand(5, &canvas);
        assert_eq!(e, Bbox::new(0, 0, 15, 15).unwrap());
        let far = Bbox::new(90, 95, 5, 5).unwrap();
        // n = 5: left/top clamp at the far corner.
        let f5 = far.expand(5, &canvas);
        assert_eq!(f5, Bbox::new(85, 90, 15, 10).unwrap());
        // n = 10: right/bottom clamp at the canvas edge instead.
        let f10 = far.expand(10, &canvas);
        assert_eq!(f10, Bbox::new(80, 85, 20, 15).unwrap());
    }

    #[test]
    fn preset_ordinals_are_stable_and_seven() {
        let all = [
            TracePreset::Draft,
            TracePreset::Wireframe,
            TracePreset::Lineart,
            TracePreset::Balanced,
            TracePreset::Detailed,
            TracePreset::HighFidelity,
            TracePreset::Pixel,
        ];
        assert_eq!(all.len(), 7);
        for (i, p) in all.iter().enumerate() {
            assert_eq!(p.ordinal(), (i + 1) as u8);
        }
    }

    #[test]
    fn group_output_helpers() {
        let out = GroupAllOutput {
            groups: vec![],
            svgs: vec![],
        };
        assert!(out.all_traced());
        assert_eq!(out.trace_successes(), 0);
    }

    /// Minimal in-memory raster used by trait-level tests (also the shape the
    /// spike's PNG-backed view must satisfy).
    struct MemRaster {
        w: u32,
        h: u32,
        luma: Vec<f32>,
    }
    impl RasterView for MemRaster {
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

    struct OnePixelGrouper;
    impl GroupingStrategy for OnePixelGrouper {
        fn group_all(&self, raster: &dyn RasterView, mask: &ForegroundMask) -> Vec<IconGroup> {
            let w = raster.width() as usize;
            for i in 0..mask.pixel_count() as usize {
                if mask.get_index(i) {
                    let x = (i % w) as u32;
                    let y = (i / w) as u32;
                    return vec![IconGroup {
                        bbox: Bbox::from_parts(x, y, 1, 1),
                        area: 1,
                        origin: (x, y),
                    }];
                }
            }
            vec![]
        }
    }

    #[test]
    fn trait_roundtrip_on_mem_raster() {
        let mut luma = vec![255.0f32; 8 * 8];
        luma[1 * 8 + 1] = 0.0;
        let raster = MemRaster { w: 8, h: 8, luma };
        let mut mask = ForegroundMask::new(8, 8);
        mask.set_index(9, true);
        let groups = OnePixelGrouper.group_all(&raster, &mask);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].bbox, Bbox::new(1, 1, 1, 1).unwrap());
        assert_eq!(groups[0].origin, (1, 1));
    }
}
