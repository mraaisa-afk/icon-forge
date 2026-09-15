//! §3.3 stage ⑧ — score + cache.
//!
//! Renders the emitted document with resvg/tiny-skia and compares it
//! against the icon crop on **ink evidence**: the reference plane is the
//! per-pixel max-component distance from the detected background colour
//! (polarity-independent — same definition family as the stage ⑤ BW
//! threshold; raw crop alpha is useless on opaque sheets), the render
//! plane is the SVG's alpha channel. Both planes are ink-centroid-aligned
//! (integer shift) before the three metrics are computed: MAE, block SSIM
//! (8×8 windows), and ink IoU (alpha ≥ 128). The composite is
//! `0.5·SSIM + 0.3·(1−MAE) + 0.2·IoU`.
//!
//! The vectorize+score payload is content-addressed through the Phase-1
//! [`CacheStore`] (key `blake3(crop) ‖ preset ‖ segParams ‖ version`,
//! zstd-compressed on disk); a corrupt payload is treated as a miss and
//! recomputed, so a bad row can never poison output.

use isg_core::{Bbox, TraceError, TracePreset};
use serde::{Deserialize, Serialize};

use super::background::BackgroundModel;
use super::emit::{emit_svg, EmitError, CACHE_VERSION};
use super::profiles;
use super::raster::SheetRaster;
use super::trace::vectorize_icon;
use crate::cache::CacheStore;
use crate::db::Library;
use crate::IsgError;

/// Ink threshold (alpha ≥ 128 counts as ink) for the IoU metric.
const INK_THRESHOLD: f64 = 128.0;
/// SSIM window size (spec uses standard 8×8 blocks for icon-scale art).
const SSIM_BLOCK: u32 = 8;

/// Stage ⑧ quality metrics, all in `[0, 1]` (higher is better).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Score {
    /// Mean absolute ink-plane error.
    pub mae: f32,
    /// Block SSIM (8×8 windows) over the ink planes.
    pub ssim: f32,
    /// Ink IoU (alpha ≥ 128) after centroid alignment.
    pub iou: f32,
    /// `0.5·SSIM + 0.3·(1−MAE) + 0.2·IoU`.
    pub composite: f32,
}

impl Score {
    /// Blends the three raw metrics into the composite score.
    #[must_use]
    pub fn new(mae: f64, ssim: f64, iou: f64) -> Self {
        let (mae, ssim, iou) = (mae as f32, ssim as f32, iou as f32);
        Score {
            mae,
            ssim,
            iou,
            composite: 0.5 * ssim + 0.3 * (1.0 - mae) + 0.2 * iou,
        }
    }
}

/// Stage ⑤–⑧ output for one icon: final SVG + palette + quality score.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScoredIcon {
    /// Validated final SVG document (stage ⑦).
    pub svg: String,
    /// Stage ④ palette (paint order).
    pub palette: Vec<[u8; 4]>,
    /// Stage ⑧ quality score against the source crop.
    pub score: Score,
}

/// Stage ⑧ scoring failure modes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScoreError {
    /// Crop length ≠ 4·w·h, or `w`/`h` is zero.
    SizeMismatch,
    /// resvg refused to render the document.
    Render(String),
}

impl std::fmt::Display for ScoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScoreError::SizeMismatch => f.write_str("crop size does not match w×h"),
            ScoreError::Render(e) => write!(f, "render failed: {e}"),
        }
    }
}

impl std::error::Error for ScoreError {}

/// Errors from the composed vectorize+score (and cached) path.
#[derive(Debug)]
pub enum VectorizeError {
    /// Stage ⑤⑥ failure.
    Trace(TraceError),
    /// Stage ⑦ failure.
    Emit(EmitError),
    /// Stage ⑧ failure.
    Score(ScoreError),
    /// Cache store failure.
    Cache(IsgError),
    /// Payload (de)serialization failure while storing.
    Json(String),
}

impl From<TraceError> for VectorizeError {
    fn from(e: TraceError) -> Self {
        VectorizeError::Trace(e)
    }
}

impl From<EmitError> for VectorizeError {
    fn from(e: EmitError) -> Self {
        VectorizeError::Emit(e)
    }
}

impl From<ScoreError> for VectorizeError {
    fn from(e: ScoreError) -> Self {
        VectorizeError::Score(e)
    }
}

impl From<IsgError> for VectorizeError {
    fn from(e: IsgError) -> Self {
        VectorizeError::Cache(e)
    }
}

impl std::fmt::Display for VectorizeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VectorizeError::Trace(e) => write!(f, "trace failed: {e}"),
            VectorizeError::Emit(e) => write!(f, "emit failed: {e}"),
            VectorizeError::Score(e) => write!(f, "score failed: {e}"),
            VectorizeError::Cache(e) => write!(f, "cache failed: {e}"),
            VectorizeError::Json(e) => write!(f, "payload serialization failed: {e}"),
        }
    }
}

impl std::error::Error for VectorizeError {}

/// Renders `doc` at `w×h` and scores it against the crop's ink evidence
/// (per-pixel max-component distance from `bg_rgba`).
pub fn score_svg(
    doc: &str,
    crop_rgba: &[u8],
    bg_rgba: [u8; 4],
    w: u32,
    h: u32,
) -> Result<Score, ScoreError> {
    if w == 0 || h == 0 || crop_rgba.len() != 4 * w as usize * h as usize {
        return Err(ScoreError::SizeMismatch);
    }
    let tree = resvg::usvg::Tree::from_str(doc, &resvg::usvg::Options::default())
        .map_err(|e| ScoreError::Render(e.to_string()))?;
    let size = tree.size();
    let sx = w as f32 / size.width();
    let sy = h as f32 / size.height();
    let mut pm = resvg::tiny_skia::Pixmap::new(w, h)
        .ok_or_else(|| ScoreError::Render("pixmap allocation failed".to_string()))?;
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(sx, sy), &mut pm.as_mut());
    // Premultiplied RGBA bytes; the alpha byte is unaffected by the
    // premultiplication.
    let render_plane: Vec<u8> = pm.data().chunks_exact(4).map(|p| p[3]).collect();
    let ref_plane: Vec<u8> = crop_rgba
        .chunks_exact(4)
        .map(|p| {
            p.iter()
                .zip(bg_rgba.iter())
                .map(|(&c, &b)| c.abs_diff(b))
                .max()
                .unwrap_or(0)
        })
        .collect();
    Ok(compare_planes(&ref_plane, &render_plane, w, h))
}

/// Runs stages ⑤–⑧ for one icon: trace → emit (usvg gate) → score.
pub fn vectorize_scored(
    sheet: &SheetRaster,
    bbox: Bbox,
    bg: &BackgroundModel,
    preset: TracePreset,
) -> Result<ScoredIcon, VectorizeError> {
    let vectors = vectorize_icon(sheet, bbox, bg, preset)?;
    let prof = profiles::profile(preset);
    let svg = emit_svg(&vectors, bbox.w, bbox.h, prof.doc_name, prof.stroke_only)?;
    let crop = sheet.crop_rgba(bbox);
    let score = score_svg(&svg, &crop, bg.rgba, bbox.w, bbox.h)?;
    Ok(ScoredIcon {
        svg,
        palette: vectors.palette,
        score,
    })
}

/// `vectorize_scored` behind the stage ⑧ cache. Returns the icon and
/// whether it was a cache hit. The payload is the [`ScoredIcon`] as JSON
/// (zstd-compressed on disk); a corrupt payload is treated as a miss.
pub fn cached_vectorize(
    store: &CacheStore,
    lib: &Library,
    sheet: &SheetRaster,
    bbox: Bbox,
    bg: &BackgroundModel,
    preset: TracePreset,
    seg_params: &str,
) -> Result<(ScoredIcon, bool), VectorizeError> {
    let crop = sheet.crop_rgba(bbox);
    let prof = profiles::profile(preset);
    let key = CacheStore::cache_key(&crop, prof.doc_name, seg_params, CACHE_VERSION);
    if let Some(bytes) = store.get(lib, &key)? {
        if let Ok(icon) = serde_json::from_slice::<ScoredIcon>(&bytes) {
            return Ok((icon, true));
        }
    }
    let icon = vectorize_scored(sheet, bbox, bg, preset)?;
    let payload = serde_json::to_vec(&icon).map_err(|e| VectorizeError::Json(e.to_string()))?;
    store.put(lib, &key, &payload)?;
    Ok((icon, false))
}

/// Alpha-weighted ink centroid; `None` when the plane has no ink.
fn alpha_centroid(plane: &[u8], w: u32, h: u32) -> Option<(f64, f64)> {
    let (mut sx, mut sy, mut sw) = (0.0f64, 0.0f64, 0.0f64);
    for y in 0..h {
        for x in 0..w {
            let a = f64::from(plane[(y * w + x) as usize]);
            if a > 0.0 {
                sx += f64::from(x) * a;
                sy += f64::from(y) * a;
                sw += a;
            }
        }
    }
    if sw == 0.0 {
        None
    } else {
        Some((sx / sw, sy / sw))
    }
}

/// Mean SSIM over 8×8 blocks (clamped partial blocks at the edges), with
/// the standard constants `C1 = (0.01·255)²`, `C2 = (0.03·255)²`. A plane
/// against itself — including an all-zero plane — scores exactly 1.
fn ssim_planes(a: &[u8], b: &[u8], w: u32, h: u32) -> f64 {
    const C1: f64 = 6.5025;
    const C2: f64 = 58.5225;
    let mut total = 0.0f64;
    let mut blocks = 0u32;
    let mut y0 = 0u32;
    while y0 < h {
        let bh = SSIM_BLOCK.min(h - y0);
        let mut x0 = 0u32;
        while x0 < w {
            let bw = SSIM_BLOCK.min(w - x0);
            let (mut sa, mut sb, mut saa, mut sbb, mut sab) = (0.0, 0.0, 0.0, 0.0, 0.0);
            for y in y0..y0 + bh {
                for x in x0..x0 + bw {
                    let i = (y * w + x) as usize;
                    let av = f64::from(a[i]);
                    let bv = f64::from(b[i]);
                    sa += av;
                    sb += bv;
                    saa += av * av;
                    sbb += bv * bv;
                    sab += av * bv;
                }
            }
            let n = f64::from(bw * bh);
            let (ma, mb) = (sa / n, sb / n);
            let (va, vb) = (saa / n - ma * ma, sbb / n - mb * mb);
            let cov = sab / n - ma * mb;
            total += ((2.0 * ma * mb + C1) * (2.0 * cov + C2))
                / ((ma * ma + mb * mb + C1) * (va + vb + C2));
            blocks += 1;
            x0 += SSIM_BLOCK;
        }
        y0 += SSIM_BLOCK;
    }
    if blocks == 0 {
        1.0
    } else {
        total / f64::from(blocks)
    }
}

/// Aligns both planes on their ink centroids (integer shift) and computes
/// MAE / SSIM / IoU plus the composite.
fn compare_planes(reference: &[u8], render: &[u8], w: u32, h: u32) -> Score {
    let shift = match (
        alpha_centroid(reference, w, h),
        alpha_centroid(render, w, h),
    ) {
        (Some(r), Some(m)) => ((r.0 - m.0).round() as i64, (r.1 - m.1).round() as i64),
        _ => (0, 0),
    };
    let at = |x: i64, y: i64| -> u8 {
        if x < 0 || y < 0 || x >= i64::from(w) || y >= i64::from(h) {
            0
        } else {
            render[(y as u32 * w + x as u32) as usize]
        }
    };
    let mut aligned = vec![0u8; reference.len()];
    for y in 0..i64::from(h) {
        for x in 0..i64::from(w) {
            aligned[(y as u32 * w + x as u32) as usize] = at(x - shift.0, y - shift.1);
        }
    }
    let (mut abs_sum, mut inter, mut union) = (0.0f64, 0u64, 0u64);
    for (a, b) in reference.iter().zip(aligned.iter()) {
        let (a, b) = (f64::from(*a), f64::from(*b));
        abs_sum += (a - b).abs();
        let (ai, bi) = (a >= INK_THRESHOLD, b >= INK_THRESHOLD);
        if ai && bi {
            inter += 1;
        }
        if ai || bi {
            union += 1;
        }
    }
    let pixels = f64::from(w * h);
    let mae = abs_sum / (pixels * 255.0);
    let iou = if union == 0 {
        1.0
    } else {
        inter as f64 / union as f64
    };
    Score::new(mae, ssim_planes(reference, &aligned, w, h), iou)
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::background::BackgroundKind;
    use super::super::trace::IconVectors;

    const SQUARE: &str = "<path d=\"M4,4L12,4L12,12L4,12Z\" fill=\"#0a0a0a\"/>";
    const BG: [u8; 4] = [255, 255, 255, 255];

    /// 16×16 white sheet with an 8×8 near-black square at (4, 4).
    fn sheet_16() -> SheetRaster {
        let mut rgba = vec![255u8; 16 * 16 * 4];
        for y in 4..12usize {
            for x in 4..12 {
                let i = (y * 16 + x) * 4;
                rgba[i..i + 3].copy_from_slice(&[10, 10, 10]);
            }
        }
        SheetRaster::from_rgba(16, 16, rgba)
    }

    fn bg() -> BackgroundModel {
        BackgroundModel {
            kind: BackgroundKind::BorderConsensus,
            rgba: BG,
            consensus: 1.0,
        }
    }

    fn doc_for(fragment: &str, w: u32, h: u32) -> String {
        let v = IconVectors {
            svg: fragment.to_string(),
            palette: Vec::new(),
        };
        emit_svg(&v, w, h, "mono-clean", false).unwrap()
    }

    fn crop_square_at4() -> Vec<u8> {
        sheet_16().crop_rgba(Bbox::new(0, 0, 16, 16).unwrap())
    }

    #[test]
    fn identical_square_scores_near_perfect() {
        let doc = doc_for(SQUARE, 16, 16);
        let s = score_svg(&doc, &crop_square_at4(), BG, 16, 16).unwrap();
        // The square is [10,10,10] on white, so reference ink is 245, not
        // 255: mae floors at 64·10/(256·255) ≈ 0.0098 even for a perfect
        // trace (CI-actual 0.009804 / ssim 0.9984 / iou 1.0 / comp 0.9963).
        assert!(s.mae < 0.02, "{s:?}");
        assert!(s.ssim > 0.99, "{s:?}");
        assert!(s.iou > 0.98, "{s:?}");
        assert!(s.composite > 0.99, "{s:?}");
    }

    #[test]
    fn blank_against_blank_is_perfect() {
        // An empty fragment cannot be emitted (stage ⑦ refuses) …
        assert_eq!(
            emit_svg(
                &IconVectors {
                    svg: String::new(),
                    palette: Vec::new()
                },
                16,
                16,
                "mono-clean",
                false
            )
            .unwrap_err(),
            EmitError::EmptyFragment
        );
        // … but a path-free yet valid document scores a blank crop perfectly.
        let doc = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"16\" height=\"16\" \
                   viewBox=\"0 0 16 16\"><title>icon</title><desc>Icon Forge</desc></svg>";
        let crop = vec![255u8; 16 * 16 * 4];
        let s = score_svg(doc, &crop, BG, 16, 16).unwrap();
        assert_eq!(s.mae.to_bits(), 0);
        assert_eq!(s.iou, 1.0);
        assert_eq!(s.ssim, 1.0);
        assert_eq!(s.composite, 1.0);
    }

    #[test]
    fn mismatched_ink_scores_low() {
        let doc = doc_for(SQUARE, 16, 16);
        // Reference ink is only a 4×4 square at (6, 6).
        let mut crop = vec![255u8; 16 * 16 * 4];
        for y in 6..10usize {
            for x in 6..10 {
                let i = (y * 16 + x) * 4;
                crop[i..i + 3].copy_from_slice(&[10, 10, 10]);
            }
        }
        let s = score_svg(&doc, &crop, BG, 16, 16).unwrap();
        assert!(s.iou < 0.5, "{s:?}");
        assert!(s.mae > 0.05, "{s:?}");
        assert!(s.composite < 0.9, "{s:?}");
    }

    #[test]
    fn centroid_alignment_recovers_translation() {
        let shifted = doc_for("<path d=\"M0,0L8,0L8,8L0,8Z\" fill=\"#0a0a0a\"/>", 16, 16);
        // Reference square sits at (8..16); the render square at (0..8).
        let mut crop = vec![255u8; 16 * 16 * 4];
        for y in 8..16usize {
            for x in 8..16 {
                let i = (y * 16 + x) * 4;
                crop[i..i + 3].copy_from_slice(&[10, 10, 10]);
            }
        }
        let s = score_svg(&shifted, &crop, BG, 16, 16).unwrap();
        assert!(s.iou > 0.98, "{s:?}");
        assert!(s.mae < 0.01, "{s:?}");
    }

    #[test]
    fn composite_uses_the_documented_weights() {
        let s = Score::new(0.5, 0.8, 0.4);
        let expected = 0.5f32 * 0.8 + 0.3 * (1.0 - 0.5f32) + 0.2 * 0.4f32;
        assert_eq!(s.composite, expected);
    }

    #[test]
    fn score_svg_is_deterministic() {
        let doc = doc_for(SQUARE, 16, 16);
        let crop = crop_square_at4();
        let a = score_svg(&doc, &crop, BG, 16, 16).unwrap();
        let b = score_svg(&doc, &crop, BG, 16, 16).unwrap();
        assert_eq!(a.mae.to_bits(), b.mae.to_bits());
        assert_eq!(a.ssim.to_bits(), b.ssim.to_bits());
        assert_eq!(a.iou.to_bits(), b.iou.to_bits());
        assert_eq!(a.composite.to_bits(), b.composite.to_bits());
    }

    #[test]
    fn bad_crop_dimensions_are_rejected() {
        let doc = doc_for(SQUARE, 16, 16);
        assert_eq!(
            score_svg(&doc, &[], BG, 0, 16).unwrap_err(),
            ScoreError::SizeMismatch
        );
        assert_eq!(
            score_svg(&doc, &[0u8; 10], BG, 16, 16).unwrap_err(),
            ScoreError::SizeMismatch
        );
    }

    #[test]
    fn unparseable_doc_is_a_render_error() {
        let crop = crop_square_at4();
        assert!(matches!(
            score_svg("not an svg", &crop, BG, 16, 16).unwrap_err(),
            ScoreError::Render(_)
        ));
    }

    #[test]
    fn vectorize_scored_end_to_end_draft() {
        let sheet = sheet_16();
        let icon = vectorize_scored(
            &sheet,
            Bbox::new(4, 4, 8, 8).unwrap(),
            &bg(),
            TracePreset::Draft,
        )
        .unwrap();
        assert!(icon.svg.starts_with("<svg xmlns="), "{}", icon.svg);
        assert!(icon.palette.is_empty(), "BW trace has no palette");
        assert!(icon.score.ssim > 0.9, "{:?}", icon.score);
        assert!(icon.score.iou > 0.9, "{:?}", icon.score);
        super::super::emit::validate(&icon.svg).unwrap();
    }

    #[test]
    fn payload_json_roundtrip_is_exact() {
        let sheet = sheet_16();
        let icon =
            vectorize_scored(&sheet, Bbox::new(4, 4, 8, 8).unwrap(), &bg(), TracePreset::Draft)
                .unwrap();
        let bytes = serde_json::to_vec(&icon).unwrap();
        let back: ScoredIcon = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back, icon);
    }

    #[test]
    fn cached_vectorize_miss_then_hit() {
        let lib = Library::open_in_memory().unwrap();
        let dir = std::env::temp_dir().join(format!("isg-score-test-{}", std::process::id()));
        let store = CacheStore::new(&dir);
        let sheet = sheet_16();
        let bbox = Bbox::new(4, 4, 8, 8).unwrap();

        let (first, hit) = cached_vectorize(
            &store, &lib, &sheet, bbox, &bg(), TracePreset::Draft, "sp1",
        )
        .unwrap();
        assert!(!hit, "first call is a miss");
        let (second, hit) = cached_vectorize(
            &store, &lib, &sheet, bbox, &bg(), TracePreset::Draft, "sp1",
        )
        .unwrap();
        assert!(hit, "second call is a hit");
        assert_eq!(first, second);

        // A different preset (different doc name → different key) misses.
        let (_, hit) = cached_vectorize(
            &store, &lib, &sheet, bbox, &bg(), TracePreset::Wireframe, "sp1",
        )
        .unwrap();
        assert!(!hit, "other preset misses");

        // A corrupt payload is treated as a miss and recomputed.
        let crop = sheet.crop_rgba(bbox);
        let key = CacheStore::cache_key(&crop, "mono-fast", "sp1", CACHE_VERSION);
        store.put(&lib, &key, b"{broken json").unwrap();
        let (third, hit) = cached_vectorize(
            &store, &lib, &sheet, bbox, &bg(), TracePreset::Draft, "sp1",
        )
        .unwrap();
        assert!(!hit, "corrupt payload is a miss");
        assert_eq!(third, first);
        let (fourth, hit) = cached_vectorize(
            &store, &lib, &sheet, bbox, &bg(), TracePreset::Draft, "sp1",
        )
        .unwrap();
        assert!(hit, "repaired payload hits again");
        assert_eq!(fourth, first);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
