//! §3.3-⑤⑥ Trace — vtracer per icon crop, plus the stage ④–⑥ composition
//! for a single icon ([`vectorize_icon`]).
//!
//! vtracer pipelines are **not `Send`**: batch parallelization (W4) builds
//! one per thread via `rayon` `map_init`. The crop fed to colour presets is
//! the stage ④ output — k-means layers composed onto the sheet background —
//! and our palette is injected into vtracer's `FixedPalette` fitter, so
//! vtracer never re-clusters beyond palette assignment. BW presets binarize
//! relative to the sheet background luma (polarity-independent), the same
//! decision the Phase 0 spike validated.

use isg_core::{BackgroundModel, Bbox, RasterView, TraceError, TracePreset};
use vtracer::{Color, ColorImage, Config};

use super::profiles;
use super::quantize::{self, Layer, QuantizeParams};
use super::raster::SheetRaster;
use super::simplify::{self, SimplifyParams};

/// Stage ⑤⑥ output for one icon: the (simplified) SVG fragment and the
/// stage ④ palette it was traced against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IconVectors {
    /// SVG fragment as produced by vtracer with every `d` simplified.
    pub svg: String,
    /// The quantized palette ([r, g, b, a] per layer, paint order).
    pub palette: Vec<[u8; 4]>,
}

/// Vectorizes one icon crop (stages ④ quantize → ⑤ trace → ⑥ simplify).
pub fn vectorize_icon(
    sheet: &SheetRaster,
    bbox: Bbox,
    bg: &BackgroundModel,
    preset: TracePreset,
) -> Result<IconVectors, TraceError> {
    if bbox.is_empty() {
        return Err(TraceError::EmptyBbox);
    }
    if bbox.x + bbox.w > sheet.width() || bbox.y + bbox.h > sheet.height() {
        return Err(TraceError::TraceFailed("bbox outside sheet".to_string()));
    }
    let prof = profiles::profile(preset);
    let mut cfg = prof.vtracer_config();
    let mut palette = Vec::new();
    let pixels;
    if prof.colour {
        let crop = sheet.crop_rgba(bbox);
        let qp = match prof.k {
            Some(k) => QuantizeParams::with_k_cap(k),
            None => QuantizeParams::default(),
        };
        let layers = quantize::quantize_crop(&crop, bbox.w, bbox.h, &qp);
        palette = layers.iter().map(|l| l.rgba).collect();
        cfg.palette = layers
            .iter()
            .map(|l| Color::new_rgba(l.rgba[0], l.rgba[1], l.rgba[2], l.rgba[3]))
            .collect();
        pixels = compose(&layers, bbox.w, bbox.h, bg.rgba);
    } else {
        pixels = bw_crop(sheet, bbox, luma_of(bg.rgba), 32.0);
    }
    let svg = svg_from(&cfg, pixels, bbox.w, bbox.h, prof.simplify)?;
    Ok(IconVectors { svg, palette })
}

/// Builds the pipeline, traces the crop, and runs stage ⑥ over every `d`.
fn svg_from(
    cfg: &Config,
    pixels: Vec<u8>,
    w: u32,
    h: u32,
    sp: SimplifyParams,
) -> Result<String, TraceError> {
    let pipeline = cfg
        .build()
        .map_err(|e| TraceError::TraceFailed(format!("pipeline: {e}")))?;
    let img = ColorImage {
        pixels,
        width: w as usize,
        height: h as usize,
    };
    let svg = pipeline
        .to_svg(&img)
        .map_err(|e| TraceError::TraceFailed(e.to_string()))?;
    rewrite_ds(&svg, sp)
}

/// Replaces every `d="…"` attribute with its stage ⑥ simplified form.
fn rewrite_ds(svg: &str, sp: SimplifyParams) -> Result<String, TraceError> {
    let mut out = String::with_capacity(svg.len());
    let mut rest = svg;
    while let Some(pos) = rest.find("d=\"") {
        out.push_str(&rest[..pos + 3]);
        rest = &rest[pos + 3..];
        let end = rest.find('"').ok_or(TraceError::UnparseableSvg)?;
        let d = &rest[..end];
        out.push_str(&simplify::simplify_d(d, &sp)?);
        rest = &rest[end..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Paints the layers (area DESC = paint order) over the background colour.
fn compose(layers: &[Layer], w: u32, h: u32, bg: [u8; 4]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 * w as usize * h as usize);
    for _ in 0..w as usize * h as usize {
        out.extend_from_slice(&bg);
    }
    for layer in layers {
        for run in layer.mask.runs() {
            let row = run.y as usize * w as usize;
            for x in run.x_start..run.x_end {
                let i = (row + x as usize) * 4;
                out[i..i + 4].copy_from_slice(&layer.rgba);
            }
        }
    }
    out
}

/// Binarizes the crop relative to the sheet's background luma: ink → black
/// on white, polarity-independent (Phase 0 spike decision).
fn bw_crop(sheet: &SheetRaster, bbox: Bbox, bg_luma: f32, deviation: f32) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 * bbox.w as usize * bbox.h as usize);
    for y in 0..bbox.h {
        let row = sheet.luma_row(bbox.y + y);
        for x in 0..bbox.w {
            let v = row[(bbox.x + x) as usize];
            let c = if (v - bg_luma).abs() >= deviation {
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

fn luma_of(rgba: [u8; 4]) -> f32 {
    0.299 * f32::from(rgba[0]) + 0.587 * f32::from(rgba[1]) + 0.114 * f32::from(rgba[2])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::background::BackgroundKind;
    use kurbo::BezPath;

    /// White 32×32 sheet with a black 8×8 square at (12, 12).
    fn bw_sheet() -> SheetRaster {
        let mut rgba = vec![255u8; 32 * 32 * 4];
        for y in 12..20 {
            for x in 12..20 {
                let i = (y * 32 + x) * 4;
                rgba[i..i + 3].copy_from_slice(&[10, 10, 10]);
            }
        }
        SheetRaster::from_rgba(32, 32, rgba)
    }

    fn white_bg() -> BackgroundModel {
        BackgroundModel {
            kind: BackgroundKind::BorderConsensus,
            rgba: [255, 255, 255, 255],
            consensus: 0.99,
        }
    }

    #[test]
    fn empty_bbox_is_rejected() {
        let sheet = bw_sheet();
        let out = vectorize_icon(&sheet, Bbox::empty(), &white_bg(), TracePreset::Draft);
        assert_eq!(out, Err(TraceError::EmptyBbox));
    }

    #[test]
    fn bbox_outside_sheet_is_rejected() {
        let sheet = bw_sheet();
        let err = vectorize_icon(
            &sheet,
            Bbox::new(30, 30, 8, 8).unwrap(),
            &white_bg(),
            TracePreset::Draft,
        )
        .unwrap_err();
        assert!(matches!(err, TraceError::TraceFailed(_)), "{err:?}");
    }

    #[test]
    fn draft_traces_square_and_simplifies() {
        let sheet = bw_sheet();
        let out = vectorize_icon(
            &sheet,
            Bbox::new(8, 8, 16, 16).unwrap(),
            &white_bg(),
            TracePreset::Draft,
        )
        .unwrap();
        assert!(out.svg.contains("<path"), "vtracer output: {}", out.svg);
        assert!(out.svg.contains("d=\""), "vtracer output: {}", out.svg);
        // Every simplified d must still parse (kurbo round-trip smoke).
        let mut rest = out.svg.as_str();
        while let Some(pos) = rest.find("d=\"") {
            rest = &rest[pos + 3..];
            let end = rest.find('"').unwrap();
            BezPath::from_svg(&rest[..end]).expect("simplified d parses");
            rest = &rest[end..];
        }
    }

    #[test]
    fn tracing_is_deterministic() {
        let sheet = bw_sheet();
        let bbox = Bbox::new(8, 8, 16, 16).unwrap();
        let bg = white_bg();
        let a = vectorize_icon(&sheet, bbox, &bg, TracePreset::Draft).unwrap();
        let b = vectorize_icon(&sheet, bbox, &bg, TracePreset::Draft).unwrap();
        assert_eq!(a.svg, b.svg);
    }

    #[test]
    fn colour_preset_quantizes_and_traces() {
        // 24×8 sheet: three 8×8 blocks (red, green, blue).
        let mut rgba = vec![255u8; 24 * 8 * 4];
        for (x0, c) in [(0, [255u8, 0, 0]), (8, [0, 255, 0]), (16, [0, 0, 255])] {
            for y in 0..8 {
                for x in x0..x0 + 8 {
                    let i = (y * 24 + x) * 4;
                    rgba[i..i + 3].copy_from_slice(&c);
                }
            }
        }
        let sheet = SheetRaster::from_rgba(24, 8, rgba);
        let bg = BackgroundModel {
            kind: BackgroundKind::KMeans,
            rgba: [255, 255, 255, 255],
            consensus: 0.9,
        };
        let out = vectorize_icon(
            &sheet,
            Bbox::new(0, 0, 24, 8).unwrap(),
            &bg,
            TracePreset::Balanced,
        )
        .unwrap();
        assert!(
            out.palette.len() >= 2 && out.palette.len() <= 8,
            "palette {:?}",
            out.palette
        );
        assert!(out.svg.contains("<path"), "{}", out.svg);
        // The three pure hues must survive quantization verbatim.
        for c in [[255, 0, 0], [0, 255, 0], [0, 0, 255]] {
            assert!(
                out.palette.iter().any(|p| &p[..3] == &c[..]),
                "{c:?} missing from {:?}",
                out.palette
            );
        }
    }

    #[test]
    fn pixel_preset_traces_lattice() {
        let sheet = bw_sheet();
        let out = vectorize_icon(
            &sheet,
            Bbox::new(8, 8, 16, 16).unwrap(),
            &white_bg(),
            TracePreset::Pixel,
        )
        .unwrap();
        assert!(out.svg.contains("<path"), "{}", out.svg);
    }
}
