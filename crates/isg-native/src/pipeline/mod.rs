//! §3.3 auto-vectorization pipeline — segmentation (stages ①–③), colour
//! quantization (④), preset-profiled vtracer tracing (⑤), the custom
//! geometry simplify pass (⑥), emit + usvg validation (⑦) and scoring +
//! content-addressed caching (⑧). Batch orchestration lands in W4.
//!
//! Every stage is byte-deterministic: same input bytes + same
//! [`SegParams`]/preset → identical output, which is what makes the
//! vectorization cache (stage ⑧) sound.

pub mod background;
pub mod batch;
pub mod clean;
pub mod containment;
pub mod emit;
pub mod grid;
pub mod group;
pub mod merge;
pub mod normalize;
pub mod profiles;
pub mod quantize;
pub mod raster;
pub mod score;
pub mod simplify;
pub mod split;
pub mod trace;

use isg_core::ForegroundMask;

use crate::IsgError;

pub use background::{BackgroundKind, BackgroundModel, SegParams};
pub use batch::{
    vectorize_sheet_batch, BatchError, BatchOptions, BatchSummary, SharedLibrary, SheetRef,
};
pub use clean::{clean, close3, dilate3, erode3, median3, open3};
pub use containment::{
    build_containment, Containment, ContainmentForest, ContainmentParams, ContainmentStats,
    CONTAINMENT_VERSION,
};
pub use emit::{emit_svg, validate, EmitError, CACHE_VERSION};
pub use grid::{detect_grid, GridFit, GridHint, GridParams, GridStats, GRID_VERSION};
pub use group::{CclGrouper, RleCclGrouper};
pub use merge::{
    refine_groups, refine_groups_with_stats, RefineParams, RefineStats, REFINE_VERSION,
};
pub use normalize::normalize;
pub use profiles::{profile, TraceProfile};
pub use quantize::{Layer, QuantizeParams};
pub use raster::SheetRaster;
pub use score::{
    cached_vectorize, vectorize_scored, Score, ScoreError, ScoredIcon, VectorizeError,
};
pub use simplify::SimplifyParams;
pub use split::{SplitParams, SplitStats, SPLIT_VERSION};
pub use trace::{vectorize_icon, IconVectors};

/// Result of the segmentation stages (①–③).
#[derive(Clone, Debug)]
pub struct SegOutput {
    /// Normalized sheet raster (RGBA8 + luma).
    pub sheet: SheetRaster,
    /// Cleaned foreground mask.
    pub mask: ForegroundMask,
    /// Detected background model.
    pub background: BackgroundModel,
}

/// Runs stages ①–③: normalize → background detect + mask → clean.
pub fn segment(bytes: &[u8], max_dim: u32, params: &SegParams) -> Result<SegOutput, IsgError> {
    let sheet = normalize(bytes, max_dim)?;
    let background = background::detect_background(&sheet, params);
    let raw = background::build_mask(&sheet, &background, params);
    let mask = clean::clean(&raw, params.close_passes);
    Ok(SegOutput {
        sheet,
        mask,
        background,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ExtendedColorType, ImageEncoder};

    /// Solid white 64×64 sheet, black 20×20 square at (20, 20), two isolated
    /// gray speckles — encoded as PNG bytes via the `image` dev encoder.
    fn sheet_bytes() -> Vec<u8> {
        let mut rgba = vec![255u8; 64 * 64 * 4];
        for y in 20usize..40 {
            for x in 20usize..40 {
                let i = (y * 64 + x) * 4;
                rgba[i..i + 3].copy_from_slice(&[10, 10, 10]);
            }
        }
        for &(x, y) in &[(5usize, 5usize), (50usize, 60usize)] {
            let i = (y * 64 + x) * 4;
            rgba[i..i + 3].copy_from_slice(&[128, 128, 128]);
        }
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&rgba, 64, 64, ExtendedColorType::Rgba8)
            .expect("png encode");
        png
    }

    #[test]
    fn segment_finds_square_and_drops_speckle() {
        let params = SegParams::default();
        let out = segment(&sheet_bytes(), 4096, &params).unwrap();
        assert_eq!(out.background.kind, BackgroundKind::BorderConsensus);
        assert_eq!(out.mask.width(), 64);
        // median(3) cuts the square's 4 corner pixels and close's erosion
        // re-cuts them, so the exact corners never come back — assert the
        // inner corners instead.
        assert!(out.mask.get(21, 21) && out.mask.get(38, 38));
        assert!(!out.mask.get(20, 20), "corner stays cut");
        assert!(!out.mask.get(5, 5), "speckle cleaned");
        assert!(!out.mask.get(50, 60), "speckle cleaned");
        // 20×20 square minus its 4 corner pixels = 396.
        assert!(
            (300..=400).contains(&out.mask.ink_count()),
            "ink {}",
            out.mask.ink_count()
        );
    }

    #[test]
    fn segment_is_byte_deterministic() {
        let params = SegParams::default();
        let bytes = sheet_bytes();
        let a = segment(&bytes, 4096, &params).unwrap();
        let b = segment(&bytes, 4096, &params).unwrap();
        assert_eq!(a.mask, b.mask);
        assert_eq!(a.sheet.rgba(), b.sheet.rgba());
        assert_eq!(
            a.background.consensus.to_bits(),
            b.background.consensus.to_bits()
        );
    }

    #[test]
    fn segment_rejects_garbage_bytes() {
        let err = segment(b"not an image", 4096, &SegParams::default()).unwrap_err();
        assert!(matches!(err, IsgError::Corrupt(_)), "{err:?}");
    }
}
