//! §3.6 review system — the three detectors, and the triage log behind them.
//!
//! A review pass answers one question per icon: *does a human need to look at
//! this?* Three detectors answer it from different angles, and this module is
//! the arithmetic of all three plus the bookkeeping of the decisions made:
//!
//! * [`quality`] — `LowQuality` / `OverComplex` / `OpenContour`, read off the
//!   vectorizer's own score and the traced outline.
//! * [`dupes`] — the three-stage duplicate cascade: perceptual hash + LSH
//!   banding, then IoU/Hausdorff verification, then blake3/SSIM confirmation.
//! * [`outliers`] — MAD-based modified z-scores over the per-icon numbers, plus
//!   modal style/palette mismatch.
//! * [`triage`] — the timestamped, undoable decision log and `review.csv`.
//!
//! The split mirrors [`crate::sheet`]: everything here is pure arithmetic over
//! planes and numbers, with no external crate and no image decoding, so it can
//! be unit-tested without a corpus. The half that needs a renderer — 2× cell
//! renders, the 64×64 normalisation the hashes consume, blake3 digests and the
//! SSIM confirmation — lives in [`crate::review_native`], and the two meet
//! through plain values ([`dupes::HashItem`], `Score`, [`outliers::IconStat`]).
//!
//! Two design rules are worth stating because they shape the API:
//!
//! * **Hashes are of the *normalised render*, never the file.** Two icons that
//!   differ only in scale, or in the odd anti-aliased edge pixel, must land in
//!   the same bucket — that is the whole point of normalising to 64×64 first.
//! * **Cheap stages may only *propose*.** The hash stage proposes candidates,
//!   IoU/Hausdorff verifies them, and only a digest or SSIM match confirms.
//!   Nothing is ever reported as a duplicate on a hash match alone, which is
//!   what keeps precision honest at the 0.90 the roadmap asks for.

pub mod dupes;
pub mod outliers;
pub mod quality;
pub mod triage;

pub use dupes::{
    a_hash, band_keys, candidate_pairs, cluster, confirm, d_hash, downscale_luma,
    hausdorff_normalised, ink_iou, suggest_keeper, verify, DupCluster, DupOptions, HashItem,
    Verified, AHASH_H, AHASH_W, DHASH_H, DHASH_W, HASH_PLANE, INK_THRESHOLD, LSH_BANDS,
};
pub use outliers::{
    mad, mean_ad, median, modal_palette, modal_style, modified_z, scan as scan_outliers, spread,
    IconStat, OutlierFlag, OutlierKind, Spread, StyleClass, FILLED_RATIO, MAD_SCALE, MEAN_AD_SCALE,
    OUTLIER_Z,
};
pub use quality::{
    flags as quality_flags, node_budget, QualityFlag, QualityInput, LOW_QUALITY_COMPOSITE,
    NODES_PER_SQRT_AREA,
};
pub use triage::{
    TriageAction, TriageDecision, TriageError, TriageLog, REVIEW_COLUMNS, REVIEW_HEADER,
};
