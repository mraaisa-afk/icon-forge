//! §3.3 auto-vectorization pipeline — raster normalization, background
//! detection, mask cleaning (stages ①–③). Quantize/trace/simplify/emit/
//! score land in W2–W3 behind the same facade.
//!
//! Every stage is byte-deterministic: same input bytes + same
//! [`SegParams`] → identical output, which is what makes the vectorization
//! cache (stage ⑧) sound.

pub mod background;
pub mod clean;
pub mod normalize;
pub mod raster;

use isg_core::ForegroundMask;

use crate::IsgError;

pub use background::{BackgroundKind, BackgroundModel, SegParams};
pub use clean::{clean, close3, dilate3, erode3, median3, open3};
pub use normalize::normalize;
pub use raster::SheetRaster;

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
    use image::{ExtendedColorType, ImageEncoder};
    use super::*;

    /// Solid white 64×64 sheet, black 20×20 square at (20, 20), two isolated
    /// gray speckles — encoded as PNG bytes via the `image` dev encoder.
    fn sheet_bytes() -> Vec<u8> {
        let mut rgba = vec![255u8; 64 * 64 * 4];
        for y in 20..40 {
            for x in 20..40 {
                let i = (y * 64 + x) * 4;
                rgba[i..i + 3].copy_from_slice(&[10, 10, 10]);
            }
        }
        for &(x, y) in &[(5u32, 5u32), (50u32, 60u32)] {
            let i = (y * 64 + x) * 4;
            rgba[i..i + 3].copy_from_slice(&[128, 128, 128]);
        }
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&rgba, ExtendedColorType::Rgba8, 64, 64)
            .expect("png encode");
        png
    }

    #[test]
    fn segment_finds_square_and_drops_speckle() {
        let params = SegParams::default();
        let out = segment(&sheet_bytes(), 4096, &params).unwrap();
        assert_eq!(out.background.kind, BackgroundKind::BorderConsensus);
        assert_eq!(out.mask.width(), 64);
        assert!(out.mask.get(20, 20) && out.mask.get(39, 39));
        assert!(!out.mask.get(5, 5), "speckle cleaned");
        assert!(!out.mask.get(50, 60), "speckle cleaned");
        // 20×20 square: median cuts 4 corners, morphology heals the core —
        // stay within sane bounds rather than pinning the exact shape.
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
        assert_eq!(a.background.consensus.to_bits(), b.background.consensus.to_bits());
    }

    #[test]
    fn segment_rejects_garbage_bytes() {
        let err = segment(b"not an image", 4096, &SegParams::default()).unwrap_err();
        assert!(matches!(err, IsgError::Corrupt(_)), "{err:?}");
    }
}
