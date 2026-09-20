//! The render half of the §3.6 review system: what the three detectors need
//! from pixels, and the pass that runs all of them over one sheet.
//!
//! The split matches [`crate::sheet`] and [`crate::sheet_native`]: the pure
//! half ([`crate::review`]) owns thresholds, statistics and the cascade's
//! control flow, and this module owns the things that need a renderer or a
//! hash — `resvg`/`tiny-skia` renders, the 64 × 64 normalised cell the hashes
//! are computed in, blake3 digests, and the wall clock.
//!
//! # The normalised cell
//!
//! Every icon is rendered into a 64 × 64 square with its **longest side
//! scaled to fill it**, aspect preserved, centred, on the sheet's background
//! colour — then reduced to an ink-evidence plane (0 where the pixel is the
//! background, up to 255 where it is maximally different from it).
//!
//! Two details are load-bearing:
//!
//! * **Aspect is preserved, not stretched.** Stretching every icon to the full
//!   square would map a 40 × 20 rectangle and a 40 × 40 square onto the same
//!   pixels, and the cascade would call them duplicates. Fitting the long side
//!   keeps a rectangle a rectangle.
//! * **The render is supersampled 2× and box-filtered down.** A one-pixel
//!   difference in where an edge lands then moves the hash bits by one, not
//!   by whether the sample happened to land on the ink. (§3.6 asks for a 2×
//!   render; this is where that resolution is spent — and the quality score
//!   below spends it again on the cell it compares.)
//!
//! The hashes, IoU and Hausdorff distance all read that one plane, so an icon's
//! "look" is defined in exactly one place.
//!
//! # What the quality flag scores against
//!
//! `LowQuality` is defined on the §3.3 stage-⑧ composite, so the number must
//! come from stage ⑧'s own entry point ([`score_svg`]) — the sheet's real crop
//! at 2× cell against the icon's own document. Re-deriving the metric here at
//! another scale would let the review panel disagree with the score the user
//! already sees next to the same icon.

use std::time::Instant;

use isg_core::{Bbox, ForegroundMask};

use crate::pipeline::raster::SheetRaster;
use crate::pipeline::score::{compare_planes, score_svg, Score};
use crate::review::dupes::{
    candidate_pairs, cluster, confirm, verify, DupCluster, DupOptions, HashItem,
};
use crate::review::outliers::{scan as scan_outliers, IconStat, OutlierFlag};
use crate::review::quality::{flags as quality_flags, QualityFlag, QualityInput};
use crate::sheet::export::artwork_from_svg;
use crate::sheet::measure;

/// Side of the normalised cell the hashes and distances are computed in.
pub const CELL: u32 = 64;

/// Supersample factor for the normalised cell: render at `CELL * 2` and box
/// them down (§3.6's 2× cell).
pub const SUPERSAMPLE: u32 = 2;

/// Scale the quality score is measured at (§3.6: "resvg render at 2× cell").
pub const QUALITY_CELL_SCALE: u32 = 2;

/// Longest side, in pixels, of the sheet a review pass accepts. The raster is
/// already normalised before it gets here; this only guards against a caller
/// handing in a full-resolution scan and allocating a 2× crop of it per icon.
pub const MAX_REVIEW_SIDE: u32 = 16_384;

/// One icon to review: where it is on the sheet, and the document it traced to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewInput {
    /// The plan's id for the icon.
    pub id: u32,
    /// The icon's cached SVG document (the traced artwork).
    pub document: String,
    /// The icon's box on the sheet raster, in pixels.
    pub bbox: Bbox,
}

/// How a review pass should behave.
#[derive(Clone, Debug, PartialEq)]
pub struct ReviewOptions {
    /// The sheet's background colour, as the renderer should composite it.
    pub background: [u8; 4],
    /// The duplicate cascade's bindings (§3.6 defaults).
    pub dupes: DupOptions,
    /// Modified z-score above which an outlier is reported (§3.6: `3.5`).
    pub outlier_z: f32,
}

impl Default for ReviewOptions {
    fn default() -> Self {
        Self {
            background: [255, 255, 255, 255],
            dupes: DupOptions::default(),
            outlier_z: crate::review::outliers::OUTLIER_Z,
        }
    }
}

/// Everything the review knows about one icon.
#[derive(Clone, Debug, PartialEq)]
pub struct IconReview {
    /// The plan's id.
    pub id: u32,
    /// Stage-⑧ score of the document against the sheet's own crop at 2× cell.
    pub score: Score,
    /// Every quality flag the icon earned, in a fixed order.
    pub flags: Vec<QualityFlag>,
    /// Segments in the traced outline.
    pub node_count: u32,
    /// True when every subpath of every shape is closed.
    pub closed: bool,
    /// Distinct colours in the traced artwork.
    pub colours: u32,
    /// The per-icon numbers the outlier detector read.
    pub stat: IconStat,
    /// dHash of the normalised cell.
    pub d_hash: u64,
    /// aHash of the normalised cell.
    pub a_hash: u64,
    /// blake3 digest of the normalised cell.
    pub digest: [u8; 32],
}

/// The duplicate cascade's tally, so a report can show its own funnel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CascadeCounts {
    /// Pairs that shared an LSH band (the propose stage).
    pub candidates: usize,
    /// Candidates that passed IoU or Hausdorff (the verify stage).
    pub verified: usize,
    /// Verified pairs that matched by digest or SSIM (the confirm stage).
    pub confirmed: usize,
}

/// One sheet's review pass.
#[derive(Clone, Debug, PartialEq)]
pub struct ReviewReport {
    /// Every icon, in the order the caller supplied them.
    pub icons: Vec<IconReview>,
    /// Duplicate clusters, keeper-first sorted.
    pub clusters: Vec<DupCluster>,
    /// Outliers, by icon id.
    pub outliers: Vec<OutlierFlag>,
    /// How many icons carry at least one quality flag.
    pub flagged: usize,
    /// The cascade's funnel.
    pub cascade: CascadeCounts,
    /// Render + hash time for the whole sheet, in milliseconds.
    pub render_ms: f64,
    /// Cascade + outlier time, in milliseconds.
    pub detect_ms: f64,
}

impl ReviewReport {
    /// The quality flags of one icon, or an empty slice when the id is unknown.
    #[must_use]
    pub fn flags_of(&self, id: u32) -> &[QualityFlag] {
        self.icons
            .iter()
            .find(|icon| icon.id == id)
            .map(|icon| icon.flags.as_slice())
            .unwrap_or(&[])
    }

    /// True when the icon is a member of a duplicate cluster.
    #[must_use]
    pub fn is_duplicate(&self, id: u32) -> bool {
        self.clusters.iter().any(|c| c.members.contains(&id))
    }
}

/// Why a review pass could not run.
#[derive(Clone, Debug, PartialEq)]
pub enum ReviewError {
    /// The document did not parse (the engine's parser said so).
    BadArtwork {
        /// Which icon.
        id: u32,
        /// The parser's reason.
        reason: String,
    },
    /// The raster and the mask are not the same size.
    SizeMismatch {
        /// The raster's dimensions.
        raster: (u32, u32),
        /// The mask's dimensions.
        mask: (u32, u32),
    },
    /// The normalised cell's side is zero or above [`MAX_REVIEW_SIDE`].
    BadSide {
        /// The side that was asked for.
        side: u32,
    },
    /// The sheet is larger than [`MAX_REVIEW_SIDE`] on a side.
    SheetTooLarge {
        /// The offending dimension.
        side: (u32, u32),
    },
    /// The icon's box is not inside the raster.
    BadBbox {
        /// Which icon.
        id: u32,
        /// The box that did not fit.
        bbox: Bbox,
    },
    /// The box contains no ink, so there is nothing to compare.
    NoInk {
        /// Which icon.
        id: u32,
    },
    /// The renderer refused the document.
    Render {
        /// Which icon.
        id: u32,
        /// The renderer's reason.
        reason: String,
    },
}

impl std::fmt::Display for ReviewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadArtwork { id, reason } => write!(f, "icon {id}: {reason}"),
            Self::SizeMismatch { raster, mask } => write!(
                f,
                "raster {}×{} and mask {}×{} differ — they must come from one normalization",
                raster.0, raster.1, mask.0, mask.1
            ),
            Self::BadSide { side } => write!(f, "cell side {side} is not a legal size"),
            Self::SheetTooLarge { side } => {
                write!(f, "sheet {}×{} exceeds the review limit", side.0, side.1)
            }
            Self::BadBbox { id, bbox } => write!(f, "icon {id}: box {bbox:?} is off the sheet"),
            Self::NoInk { id } => write!(f, "icon {id}: no ink in its box"),
            Self::Render { id, reason } => write!(f, "icon {id}: {reason}"),
        }
    }
}

impl std::error::Error for ReviewError {}

/// Renders `document` into a `side × side` ink-evidence plane: longest side
/// fitted, aspect preserved, centred, composited over `background`, then
/// reduced to the distance from that background.
///
/// The plane is what every hash and distance in the cascade reads, so this is
/// the single definition of "what this icon looks like" in the review.
///
/// # Errors
///
/// [`ReviewError::Render`] when the document does not parse or the pixmap
/// cannot be allocated, and [`ReviewError::BadSide`] for a zero side or one
/// above [`MAX_REVIEW_SIDE`].
pub fn normalized_plane(
    document: &str,
    side: u32,
    background: [u8; 4],
    id: u32,
) -> Result<Vec<u8>, ReviewError> {
    if side == 0 || side > MAX_REVIEW_SIDE {
        return Err(ReviewError::BadSide { side });
    }
    let tree =
        resvg::usvg::Tree::from_str(document, &resvg::usvg::Options::default()).map_err(|e| {
            ReviewError::Render {
                id,
                reason: e.to_string(),
            }
        })?;
    let size = tree.size();
    let (w, h) = (size.width(), size.height());
    if !(w > 0.0 && h > 0.0) {
        return Err(ReviewError::Render {
            id,
            reason: "the document has no area".to_string(),
        });
    }
    let canvas = side * SUPERSAMPLE;
    // Fit the longest side; the shorter one follows, so a rectangle stays one.
    let scale = canvas as f32 / w.max(h);
    let (dw, dh) = (w * scale, h * scale);
    let (tx, ty) = ((canvas as f32 - dw) / 2.0, (canvas as f32 - dh) / 2.0);
    let mut pixmap =
        resvg::tiny_skia::Pixmap::new(canvas, canvas).ok_or_else(|| ReviewError::Render {
            id,
            reason: format!("{canvas}×{canvas} pixmap allocation failed"),
        })?;
    let transform = resvg::tiny_skia::Transform::from_row(scale, 0.0, 0.0, scale, tx, ty);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    // Evidence plane, by the same rule stage ⑧ uses: composite the render over
    // the background, then take the largest per-channel distance from it. A
    // transparent pixel therefore scores 0 — the background is not ink.
    let bg = background;
    let plane: Vec<u8> = pixmap
        .data()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| {
            let inv = 255 - u16::from(p[3]);
            let mix = |c: u8, b: u8| u16::from(c) + (u16::from(b) * inv) / 255;
            let ev = [
                mix(p[0], bg[0]).abs_diff(u16::from(bg[0])),
                mix(p[1], bg[1]).abs_diff(u16::from(bg[1])),
                mix(p[2], bg[2]).abs_diff(u16::from(bg[2])),
            ];
            u8::try_from(ev.iter().max().copied().unwrap_or(0)).unwrap_or(255)
        })
        .collect();
    // Supersampled: box-filter down to the cell the cascade works in.
    crate::review::dupes::downscale_luma(&plane, canvas, canvas, side, side).ok_or_else(|| {
        ReviewError::Render {
            id,
            reason: "cell downscale failed".to_string(),
        }
    })
}

/// blake3 of a normalised plane — the cascade's confirm-by-identity stage.
#[must_use]
pub fn plane_digest(plane: &[u8]) -> [u8; 32] {
    *blake3::hash(plane).as_bytes()
}

/// Nearest-neighbour upscale of an RGBA crop, `factor` times on each side.
///
/// The quality score compares a 2× cell render against the sheet's own crop,
/// and nearest-neighbour is the only upscale that adds no invented detail:
/// bilinear would blur the reference and quietly raise the score of a bad
/// trace that happened to be smeared the same way.
#[must_use]
pub fn upscale_nearest(rgba: &[u8], w: u32, h: u32, factor: u32) -> Vec<u8> {
    let (tw, th) = (w * factor, h * factor);
    let mut out = Vec::with_capacity(4 * tw as usize * th as usize);
    for y in 0..th {
        for x in 0..tw {
            let (sx, sy) = (x / factor, y / factor);
            let at = ((sy as usize * w as usize) + sx as usize) * 4;
            out.extend_from_slice(&rgba[at..at + 4]);
        }
    }
    out
}

/// Counts the outline's segments and whether every subpath is closed.
fn outline_stats(artwork: &crate::sheet::export::Artwork) -> (u32, bool) {
    let mut nodes = 0u32;
    let mut closed = true;
    for shape in &artwork.shapes {
        for subpath in &shape.path {
            nodes = nodes.saturating_add(subpath.segs.len() as u32);
            closed &= subpath.closed;
        }
    }
    (nodes, closed)
}

/// A stable hash of an icon's palette: the distinct fills, sorted, blake3'd
/// down to 64 bits. Two icons with the same colours in a different order are
/// the same palette, which is what the outlier detector compares.
fn palette_hash(artwork: &crate::sheet::export::Artwork) -> u64 {
    let mut fills: Vec<[u8; 4]> = artwork.shapes.iter().map(|s| s.fill).collect();
    fills.sort_unstable();
    fills.dedup();
    let mut bytes = Vec::with_capacity(fills.len() * 4);
    for fill in &fills {
        bytes.extend_from_slice(fill);
    }
    let digest = blake3::hash(&bytes);
    let mut out = [0u8; 8];
    out.copy_from_slice(&digest.as_bytes()[..8]);
    u64::from_be_bytes(out)
}

/// Runs the duplicate cascade over already-normalised planes.
///
/// `planes` runs parallel to `items` (each entry a [`CELL`]² plane);
/// `scores` runs parallel too and orders the keeper suggestion. The cascade's
/// control flow lives in [`crate::review::dupes`] — this only feeds it pixels.
///
/// Infallible on purpose: a pair whose planes do not compare (one of them has
/// no ink at all) is *skipped*, not an error, and the funnel counts make that
/// visible instead of silent.
#[must_use]
pub fn duplicate_cascade(
    items: &[HashItem],
    planes: &[Vec<u8>],
    scores: &[f32],
    options: &ReviewOptions,
) -> (Vec<DupCluster>, CascadeCounts) {
    let mut counts = CascadeCounts::default();
    let candidates = candidate_pairs(items, crate::review::dupes::LSH_BANDS);
    counts.candidates = candidates.len();
    let mut confirmed_pairs: Vec<(usize, usize)> = Vec::new();
    for (a, b) in candidates {
        let (pa, pb) = (&planes[a], &planes[b]);
        let Some(verified) = verify(pa, pb, CELL, CELL, &options.dupes) else {
            // No ink on one side: not a duplicate, just empty. (A silently
            // skipped pair is why the counts are reported at all.)
            continue;
        };
        if !verified.pass {
            continue;
        }
        counts.verified += 1;
        let ssim = f64::from(compare_planes(pa, pb, CELL, CELL).ssim);
        if confirm(&items[a].digest, &items[b].digest, ssim, &options.dupes) {
            counts.confirmed += 1;
            confirmed_pairs.push((a, b));
        }
    }
    (cluster(items, &confirmed_pairs, scores), counts)
}

/// Reviews one sheet: quality flags, outliers and duplicates, in that order.
///
/// The raster must be the normalised sheet the mask was cut from — the crops
/// scored here are the pixels the grouping saw, so a review can never disagree
/// with the grouping about where an icon is.
///
/// # Errors
///
/// [`ReviewError::SizeMismatch`] when the raster and mask differ,
/// [`ReviewError::SheetTooLarge`] past [`MAX_REVIEW_SIDE`],
/// [`ReviewError::BadArtwork`] for a document the engine cannot parse,
/// [`ReviewError::BadBbox`] for a box off the sheet,
/// [`ReviewError::NoInk`] for a group with no ink, and [`ReviewError::Render`]
/// when a document cannot be rendered.
pub fn review_sheet(
    sheet: &SheetRaster,
    mask: &ForegroundMask,
    items: &[ReviewInput],
    options: &ReviewOptions,
) -> Result<ReviewReport, ReviewError> {
    let raster = (sheet.width(), sheet.height());
    let mask_size = (mask.width(), mask.height());
    if raster != mask_size {
        return Err(ReviewError::SizeMismatch {
            raster,
            mask: mask_size,
        });
    }
    if raster.0 > MAX_REVIEW_SIDE || raster.1 > MAX_REVIEW_SIDE {
        return Err(ReviewError::SheetTooLarge { side: raster });
    }

    let started = Instant::now();
    let mut icons = Vec::with_capacity(items.len());
    let mut planes = Vec::with_capacity(items.len());
    let mut hash_items = Vec::with_capacity(items.len());
    let mut stats = Vec::with_capacity(items.len());
    let mut flagged = 0usize;

    for item in items {
        let rect = item.bbox;
        if rect.w == 0
            || rect.h == 0
            || rect.x.saturating_add(rect.w) > sheet.width()
            || rect.y.saturating_add(rect.h) > sheet.height()
        {
            return Err(ReviewError::BadBbox {
                id: item.id,
                bbox: rect,
            });
        }
        let artwork = artwork_from_svg(item.id, format!("icon-{:04}", item.id), &item.document)
            .map_err(|e| ReviewError::BadArtwork {
                id: item.id,
                reason: e.to_string(),
            })?;
        let metrics = measure(mask, rect).ok_or(ReviewError::NoInk { id: item.id })?;

        // --- stage ⑧ at 2× cell: the number `LowQuality` is defined on ------
        let crop = sheet.crop_rgba(rect);
        let scale = QUALITY_CELL_SCALE;
        let big = upscale_nearest(&crop, rect.w, rect.h, scale);
        let score = score_svg(
            &item.document,
            &big,
            options.background,
            rect.w * scale,
            rect.h * scale,
        )
        .map_err(|e| ReviewError::Render {
            id: item.id,
            reason: e.to_string(),
        })?;

        let (node_count, closed) = outline_stats(&artwork);
        let colours = {
            let mut fills: Vec<[u8; 4]> = artwork.shapes.iter().map(|s| s.fill).collect();
            fills.sort_unstable();
            fills.dedup();
            fills.len() as u32
        };
        let fill_ratio = if rect.w == 0 || rect.h == 0 {
            0.0
        } else {
            metrics.ink_area as f32 / (rect.w as f32 * rect.h as f32)
        };
        let stat = IconStat {
            id: item.id,
            ink_size: (metrics.ink_area as f32).sqrt(),
            stroke: metrics.stroke,
            node_count: node_count as f32,
            colours: colours as f32,
            solidity: metrics.solidity,
            fill_ratio,
            palette: palette_hash(&artwork),
        };
        let flags = quality_flags(&QualityInput {
            id: item.id,
            composite: score.composite,
            node_count,
            ink_area: metrics.ink_area,
            closed,
        });
        if !flags.is_empty() {
            flagged += 1;
        }

        // --- the cascade's inputs: one normalised cell per icon ------------
        let plane = normalized_plane(&item.document, CELL, options.background, item.id)?;
        // A plane of the right size is what the hash functions promise, so the
        // `unwrap_or(0)` here is unreachable by construction; it exists because
        // a hash of zero is the honest answer for "no plane" (and every hash
        // comparison with zero simply finds nothing).
        let d_hash = crate::review::dupes::d_hash(&plane, CELL, CELL).unwrap_or(0);
        let a_hash = crate::review::dupes::a_hash(&plane, CELL, CELL).unwrap_or(0);
        let digest = plane_digest(&plane);
        hash_items.push(HashItem {
            id: item.id,
            d: d_hash,
            a: a_hash,
            digest,
        });
        stats.push(stat);
        icons.push(IconReview {
            id: item.id,
            score,
            flags,
            node_count,
            closed,
            colours,
            stat,
            d_hash,
            a_hash,
            digest,
        });
        planes.push(plane);
    }
    let render_ms = started.elapsed().as_secs_f64() * 1000.0;

    let detect_started = Instant::now();
    let scores: Vec<f32> = icons.iter().map(|icon| icon.score.composite).collect();
    let (clusters, cascade) = duplicate_cascade(&hash_items, &planes, &scores, options);
    let outliers = scan_outliers(&stats, options.outlier_z);
    let detect_ms = detect_started.elapsed().as_secs_f64() * 1000.0;

    Ok(ReviewReport {
        icons,
        clusters,
        outliers,
        flagged,
        cascade,
        render_ms,
        detect_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_upscale_repeats_every_pixel() {
        let one = vec![1, 2, 3, 4];
        assert_eq!(
            upscale_nearest(&one, 1, 1, 2),
            vec![1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4]
        );
        assert_eq!(upscale_nearest(&one, 1, 1, 1), one);
    }

    #[test]
    fn a_zero_sided_plane_is_refused_rather_than_allocated() {
        assert_eq!(
            normalized_plane("<svg/>", 0, [255, 255, 255, 255], 7),
            Err(ReviewError::BadSide { side: 0 })
        );
        assert_eq!(
            normalized_plane("<svg/>", MAX_REVIEW_SIDE + 1, [255, 255, 255, 255], 7),
            Err(ReviewError::BadSide {
                side: MAX_REVIEW_SIDE + 1
            })
        );
    }

    #[test]
    fn an_empty_sheet_reviews_to_nothing_at_all() {
        let sheet = SheetRaster::from_rgba(4, 4, vec![255; 4 * 4 * 4]);
        let mask = ForegroundMask::new(4, 4);
        let report = review_sheet(&sheet, &mask, &[], &ReviewOptions::default())
            .expect("an empty sheet is a legal review");
        assert!(report.icons.is_empty());
        assert!(report.clusters.is_empty());
        assert!(report.outliers.is_empty());
        assert_eq!(report.flagged, 0);
        assert_eq!(report.cascade, CascadeCounts::default());
    }

    #[test]
    fn a_document_the_renderer_rejects_names_its_icon() {
        let error = normalized_plane("not svg at all", CELL, [255, 255, 255, 255], 42);
        assert!(matches!(error, Err(ReviewError::Render { id: 42, .. })));
    }

    #[test]
    fn plane_digest_is_stable_and_content_addressed() {
        assert_eq!(plane_digest(&[0; 16]), plane_digest(&[0; 16]));
        assert_ne!(plane_digest(&[0; 16]), plane_digest(&[1; 16]));
    }
}
