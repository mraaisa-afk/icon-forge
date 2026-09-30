//! The review system's host surface: library rows in, decisions out.
//!
//! [`crate::review_native`] renders one icon per cell and answers with a report
//! keyed by `u32`; the library keys every icon by its 16-byte id and keeps the
//! traced documents in the stage ⑧ cache. This module is that seam. It
//!
//! * turns [`IconVectorRow`]s and their cached documents into review inputs,
//! * maps the report back onto the real ids — clusters, keepers, outliers,
//! * and states what a triage decision means for `icons.review_state`, so the
//!   review workspace and the rest of the app cannot disagree about an icon.
//!
//! Two rules shape the whole module.
//!
//! **A bad row must not hide the sheet.** A document that never reached the
//! cache, a box that fell off the raster, a cell with no ink, a document the
//! renderer refuses — each is *skipped with a reason* rather than raised, because
//! the answer the caller asked for is the review of the other 999 icons. Only
//! sheet-level failures still abort the pass: a raster and a mask of different
//! sizes, or a sheet past [`crate::review_native::MAX_REVIEW_SIDE`].
//!
//! **The `u32` is the icon's position in the sheet's row list, and it does not
//! move.** The detectors key everything by it, the triage log records decisions
//! under it, and `review.csv` writes it out — so it has to mean the same thing in
//! the next pass as in this one. Renumbering after a skip would be the quiet
//! version of that bug: pass 1 reviews ten icons with icon 4 refused, pass 2
//! reviews ten with all of them, and every decision taken in between now points
//! at the wrong drawing. Skipped icons therefore leave a *gap* in the ids rather
//! than closing it.

use std::collections::BTreeSet;

use isg_core::{Bbox, ForegroundMask};

use crate::cache::CacheStore;
use crate::db::{IconVectorRow, Library, ReviewState};
use crate::pipeline::raster::SheetRaster;
use crate::pipeline::score::ScoredIcon;
use crate::review::triage::{TriageAction, TriageDecision};
use crate::review::OutlierFlag;
use crate::review_native::{
    review_sheet, CascadeCounts, IconReview, ReviewError, ReviewInput, ReviewOptions, ReviewReport,
};
use crate::sheet::metrics::measure;

/// One icon as the review sees it: the library's id, its box, and the document
/// it traced to (absent when the cache has no payload for it yet).
///
/// The box is the raw `(x, y, w, h)` the library stored rather than a [`Bbox`]:
/// a row with an empty box is a row to *skip*, and the list has to keep every row
/// in it — including the unusable ones — or the ids after it would move.
#[derive(Clone, Debug, PartialEq)]
pub struct HostIcon {
    /// The library's 16-byte id.
    pub id: [u8; 16],
    /// The icon's box on the sheet raster, as the library stored it.
    pub bbox: (u32, u32, u32, u32),
    /// The traced document, or `None` when stage ⑧ never stored one.
    pub document: Option<String>,
}

/// Why one icon was left out of a review pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostSkip {
    /// The library's id.
    pub id: [u8; 16],
    /// What the pass saw instead of a reviewable icon.
    pub reason: String,
}

/// One reviewed icon, with the id the library knows it by.
#[derive(Clone, Debug, PartialEq)]
pub struct HostIconReview {
    /// The library's id.
    pub id: [u8; 16],
    /// The detectors' key for it: its position in the sheet's row list.
    pub index: u32,
    /// Everything the review measured.
    pub review: IconReview,
}

/// A duplicate cluster, in library ids.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostCluster {
    /// Member ids, ascending (by the pass's own order).
    pub members: Vec<[u8; 16]>,
    /// The suggested keeper.
    pub keeper: [u8; 16],
}

/// One outlier, in library ids.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HostOutlier {
    /// The library's id.
    pub id: [u8; 16],
    /// The deviation, whose own `id` field is the detectors' index.
    pub flag: OutlierFlag,
}

/// One sheet's review, keyed by the library's ids.
#[derive(Clone, Debug, PartialEq)]
pub struct HostReview {
    /// Every reviewed icon, in the sheet's row order.
    pub icons: Vec<HostIconReview>,
    /// Every icon the pass could not review, with the reason.
    pub skipped: Vec<HostSkip>,
    /// Duplicate clusters.
    pub clusters: Vec<HostCluster>,
    /// Outliers.
    pub outliers: Vec<HostOutlier>,
    /// How many icons carry at least one quality flag.
    pub flagged: usize,
    /// The cascade's funnel.
    pub cascade: CascadeCounts,
    /// Render + hash time for the whole sheet, in milliseconds.
    pub render_ms: f64,
    /// Cascade + outlier time, in milliseconds.
    pub detect_ms: f64,
}

impl HostReview {
    /// A pass that reviewed nothing, with the skips that explain why.
    #[must_use]
    pub fn empty(skipped: Vec<HostSkip>) -> Self {
        Self {
            icons: Vec::new(),
            skipped,
            clusters: Vec::new(),
            outliers: Vec::new(),
            flagged: 0,
            cascade: CascadeCounts::default(),
            render_ms: 0.0,
            detect_ms: 0.0,
        }
    }

    /// The ids of every duplicate cluster's members, as a set — what a UI dims
    /// or groups when it draws the sheet.
    #[must_use]
    pub fn duplicate_ids(&self) -> BTreeSet<[u8; 16]> {
        self.clusters
            .iter()
            .flat_map(|cluster| cluster.members.iter().copied())
            .collect()
    }
}

/// The review inputs for one sheet's icon rows — **one entry per row**, in the
/// order the library returns them, because the position is the id the detectors
/// and the triage log use.
///
/// A document is read from the payload the vectorizer already wrote, so a sheet
/// that has been traced costs no extra render to review. A row whose box is
/// unusable or whose document is missing still gets an entry here; deciding to
/// skip it is [`host_review`]'s job, where the skip can leave a gap instead of
/// shifting everything after it.
pub fn host_inputs(
    rows: &[IconVectorRow],
    store: &CacheStore,
    lib: &Library,
) -> Result<Vec<HostIcon>, crate::IsgError> {
    let mut icons = Vec::with_capacity(rows.len());
    for row in rows {
        icons.push(HostIcon {
            id: row.id,
            bbox: row.bbox,
            document: cached_document(store, lib, &row.svg_key)?,
        });
    }
    Ok(icons)
}

/// The document behind a stage ⑧ cache key, or `None` when there is nothing
/// usable under it (an empty key, a missing payload, or a payload that is not
/// the JSON [`ScoredIcon`] the vectorizer writes).
///
/// A payload that fails to parse is deliberately *not* an error: a cache entry
/// from an older build is a row without a reviewable document, not a reason to
/// fail a sheet.
pub fn cached_document(
    store: &CacheStore,
    lib: &Library,
    key: &str,
) -> Result<Option<String>, crate::IsgError> {
    if key.is_empty() {
        return Ok(None);
    }
    let Some(bytes) = store.get(lib, key)? else {
        return Ok(None);
    };
    Ok(serde_json::from_slice::<ScoredIcon>(&bytes)
        .ok()
        .map(|icon| icon.svg))
}

/// Reviews one sheet's icons and maps the report back onto the library's ids.
///
/// Ids are positions in `icons` — the sheet's own row order — and a skipped row
/// leaves a gap rather than moving the rows after it.
pub fn host_review(
    sheet: &SheetRaster,
    mask: &ForegroundMask,
    icons: &[HostIcon],
    options: &ReviewOptions,
) -> Result<HostReview, ReviewError> {
    let mut skipped = Vec::new();
    // The refusals that are cheap to see coming, so the pass below is not
    // repeated for them: an empty box, no document, a box off the raster, a box
    // with no ink.
    let mut inputs: Vec<ReviewInput> = Vec::with_capacity(icons.len());
    for (index, icon) in icons.iter().enumerate() {
        let (x, y, w, h) = icon.bbox;
        let Some(bbox) = Bbox::new(x, y, w, h) else {
            skipped.push(HostSkip {
                id: icon.id,
                reason: format!("box {x},{y} {w}×{h} is empty"),
            });
            continue;
        };
        let reason = if icon.document.is_none() {
            Some("no traced document in the cache".to_string())
        } else if bbox.x.saturating_add(bbox.w) > sheet.width()
            || bbox.y.saturating_add(bbox.h) > sheet.height()
        {
            Some(format!(
                "box {bbox:?} is off the {}×{} sheet",
                sheet.width(),
                sheet.height()
            ))
        } else if measure(mask, bbox).is_none() {
            Some("no ink in its box".to_string())
        } else {
            None
        };
        match reason {
            Some(reason) => skipped.push(HostSkip {
                id: icon.id,
                reason,
            }),
            None => inputs.push(ReviewInput {
                id: index as u32,
                document: icon
                    .document
                    .clone()
                    .expect("the branch above keeps only icons with a document"),
                bbox,
            }),
        }
    }
    if inputs.is_empty() {
        return Ok(HostReview::empty(skipped));
    }

    let report = loop {
        match review_sheet(sheet, mask, &inputs, options) {
            Ok(report) => break report,
            Err(error) => {
                // A document that exists but does not parse or render is the one
                // refusal that cannot be seen without trying. Drop that icon and
                // run the rest rather than failing the sheet; a `u32` that is not
                // one of ours, or a sheet-level error, still propagates.
                let Some((index, reason)) = per_icon_refusal(&error) else {
                    return Err(error);
                };
                let Some(at) = inputs.iter().position(|input| input.id == index) else {
                    return Err(error);
                };
                let Some(icon) = icons.get(index as usize) else {
                    return Err(error);
                };
                skipped.push(HostSkip {
                    id: icon.id,
                    reason,
                });
                inputs.remove(at);
                if inputs.is_empty() {
                    return Ok(HostReview::empty(skipped));
                }
            }
        }
    };

    Ok(map_report(&report, icons, skipped))
}

/// Which icon a per-icon refusal names, and why.
///
/// `None` for the sheet-level errors, which no amount of skipping can fix.
fn per_icon_refusal(error: &ReviewError) -> Option<(u32, String)> {
    match error {
        ReviewError::BadArtwork { id, reason } | ReviewError::Render { id, reason } => {
            Some((*id, reason.clone()))
        }
        ReviewError::BadBbox { id, bbox } => Some((*id, format!("box {bbox:?} is off the sheet"))),
        ReviewError::NoInk { id } => Some((*id, "no ink in its box".to_string())),
        ReviewError::SizeMismatch { .. }
        | ReviewError::BadSide { .. }
        | ReviewError::SheetTooLarge { .. } => None,
    }
}

/// The library-id view of a report whose ids are positions in `icons`.
///
/// A report can only name ids it was given, so every lookup here is a hit; an
/// id that is not is a bug in the pass, and panicking on it is how the next
/// person finds out rather than getting a silently wrong icon.
fn map_report(report: &ReviewReport, icons: &[HostIcon], skipped: Vec<HostSkip>) -> HostReview {
    let id_of = |index: u32| -> [u8; 16] {
        icons
            .get(index as usize)
            .unwrap_or_else(|| panic!("the review reported an id it was not given: {index}"))
            .id
    };
    HostReview {
        icons: report
            .icons
            .iter()
            .map(|review| HostIconReview {
                id: id_of(review.id),
                index: review.id,
                review: review.clone(),
            })
            .collect(),
        skipped,
        clusters: report
            .clusters
            .iter()
            .map(|cluster| HostCluster {
                members: cluster.members.iter().map(|id| id_of(*id)).collect(),
                keeper: id_of(cluster.keeper),
            })
            .collect(),
        outliers: report
            .outliers
            .iter()
            .map(|flag| HostOutlier {
                id: id_of(flag.id),
                flag: *flag,
            })
            .collect(),
        flagged: report.flagged,
        cascade: report.cascade,
        render_ms: report.render_ms,
        detect_ms: report.detect_ms,
    }
}

/// The library state a triage decision puts an icon in.
///
/// The review workspace's keys and the library's `review_state` are two views of
/// one decision, so this mapping is stated once, here, rather than at each call
/// site: `A` (and `Shift+A`, which is the same decision made in bulk) approves,
/// `R` rejects, `F` flags for another look, `D` marks a duplicate.
#[must_use]
pub const fn review_state_for(action: TriageAction) -> ReviewState {
    match action {
        TriageAction::Approve | TriageAction::BulkApprove => ReviewState::Approved,
        TriageAction::Reject => ReviewState::Rejected,
        TriageAction::Flag => ReviewState::Flagged,
        TriageAction::Duplicate => ReviewState::Duplicate,
    }
}

/// The state to restore when a decision is undone: the one it replaced, or
/// `pending` when the icon had not been decided before.
#[must_use]
pub const fn restored_review_state(previous: Option<TriageDecision>) -> ReviewState {
    match previous {
        Some(decision) => review_state_for(decision.action),
        None => ReviewState::Pending,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::triage::TriageLog;

    // `r##` because the documents contain `"#` (the hex colours), which would
    // close an `r#`-delimited raw string.
    const RECT: &str = concat!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64" viewBox="0 0 64 64">"##,
        r##"<rect x="4" y="4" width="56" height="56" fill="#000000"/></svg>"##
    );
    const SMALLER: &str = concat!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64" viewBox="0 0 64 64">"##,
        r##"<rect x="16" y="16" width="32" height="32" fill="#000000"/></svg>"##
    );
    /// A plus whose ink box is the same 56 × 56 square [`RECT`]'s is, so the two
    /// fill the review's cell as *different* artwork.
    const PLUS: &str = concat!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64" viewBox="0 0 64 64">"##,
        r##"<path d="M28 4 H36 V28 H60 V36 H36 V60 H28 V36 H4 V28 H28 Z" fill="#000000"/></svg>"##
    );

    /// A white sheet with the mask set inside each given box.
    fn sheet_and_mask(side: u32, ink: &[Bbox]) -> (SheetRaster, ForegroundMask) {
        let mut mask = ForegroundMask::new(side, side);
        for bbox in ink {
            for y in bbox.y..(bbox.y + bbox.h).min(side) {
                for x in bbox.x..(bbox.x + bbox.w).min(side) {
                    mask.set(x, y, true);
                }
            }
        }
        let raster = SheetRaster::from_rgba(side, side, vec![255u8; (side * side * 4) as usize]);
        (raster, mask)
    }

    fn id(n: u8) -> [u8; 16] {
        let mut out = [0u8; 16];
        out[0] = n;
        out[15] = n.wrapping_mul(7);
        out
    }

    fn boxed(x: u32, y: u32) -> Bbox {
        Bbox::new(x, y, 32, 32).expect("legal box")
    }

    fn icon(n: u8, xy: (u32, u32), document: Option<&str>) -> HostIcon {
        HostIcon {
            id: id(n),
            bbox: (xy.0, xy.1, 32, 32),
            document: document.map(str::to_string),
        }
    }

    fn options() -> ReviewOptions {
        ReviewOptions {
            background: [255, 255, 255, 255],
            ..ReviewOptions::default()
        }
    }

    #[test]
    fn a_duplicate_cluster_comes_back_in_library_ids() {
        // Two pairs, each pair one document used twice, and the two documents
        // *different* artwork. Two sizes of one shape would not do: the review
        // fits its cell to the icon's ink box, so a 56 px square and a 32 px
        // square are one duplicate class by design and both pairs would merge
        // (which is how the first version of this fixture failed).
        let boxes = [boxed(0, 0), boxed(32, 0), boxed(0, 32), boxed(32, 32)];
        let (sheet, mask) = sheet_and_mask(64, &boxes);
        let icons = vec![
            icon(1, (0, 0), Some(RECT)),
            icon(2, (32, 0), Some(RECT)),
            icon(3, (0, 32), Some(PLUS)),
            icon(4, (32, 32), Some(PLUS)),
        ];
        let review = host_review(&sheet, &mask, &icons, &options()).expect("a pass");
        assert_eq!(review.skipped, Vec::new());
        assert_eq!(review.icons.len(), 4);
        // The index is the icon's position in the row list, so the report can be
        // joined back to the rows it came from.
        for (position, entry) in review.icons.iter().enumerate() {
            assert_eq!(entry.index, position as u32);
            assert_eq!(entry.id, icons[position].id);
        }
        // Two documents, so two clusters, and every member is a real id.
        assert_eq!(review.clusters.len(), 2);
        let mut members: Vec<Vec<[u8; 16]>> = review
            .clusters
            .iter()
            .map(|cluster| cluster.members.clone())
            .collect();
        members.sort();
        assert_eq!(members, vec![vec![id(1), id(2)], vec![id(3), id(4)]]);
        for cluster in &review.clusters {
            assert!(
                cluster.members.contains(&cluster.keeper),
                "the keeper is a member: {cluster:?}"
            );
        }
        assert_eq!(review.duplicate_ids().len(), 4);
    }

    #[test]
    fn a_skipped_row_does_not_renumber_the_rows_after_it() {
        // The rule the triage log depends on: icon 2 is refused, and icon 3 is
        // still icon 3 afterwards — otherwise every decision taken between two
        // passes would point at the wrong drawing.
        let boxes = [boxed(0, 0), boxed(32, 0), boxed(0, 32)];
        let (sheet, mask) = sheet_and_mask(64, &boxes);
        let icons = vec![
            icon(1, (0, 0), Some(RECT)),
            icon(2, (32, 0), None),
            icon(3, (0, 32), Some(SMALLER)),
        ];
        let review = host_review(&sheet, &mask, &icons, &options()).expect("a pass");
        let pairs: Vec<(u32, [u8; 16])> = review
            .icons
            .iter()
            .map(|entry| (entry.index, entry.id))
            .collect();
        assert_eq!(pairs, vec![(0, id(1)), (2, id(3))]);
        assert_eq!(review.skipped.len(), 1);
        assert_eq!(review.skipped[0].id, id(2));
    }

    #[test]
    fn a_document_the_renderer_refuses_does_not_hide_the_others() {
        let boxes = [boxed(0, 0), boxed(32, 0), boxed(0, 32)];
        let (sheet, mask) = sheet_and_mask(64, &boxes);
        let icons = vec![
            icon(1, (0, 0), Some(RECT)),
            // A malformed `d`: the engine's parser refuses it, which is the one
            // refusal that cannot be seen without trying to render. (A document
            // that parses but draws nothing is *reviewed* — its cell is empty,
            // which is a fact about the artwork, not a broken row.)
            icon(2, (32, 0), Some(r#"<svg><path d="M 0 0 L"/></svg>"#)),
            icon(3, (0, 32), Some(SMALLER)),
        ];
        let review = host_review(&sheet, &mask, &icons, &options()).expect("a pass");
        assert_eq!(review.icons.len(), 2, "the other two are still reviewed");
        assert_eq!(review.skipped.len(), 1);
        assert_eq!(review.skipped[0].id, id(2));
        let indices: Vec<u32> = review.icons.iter().map(|entry| entry.index).collect();
        assert_eq!(indices, vec![0, 2]);
        assert_eq!(review.icons[1].id, id(3));
    }

    #[test]
    fn a_box_off_the_sheet_or_without_ink_is_skipped() {
        let boxes = [boxed(0, 0)];
        let (sheet, mask) = sheet_and_mask(64, &boxes);
        let off = HostIcon {
            id: id(9),
            bbox: (48, 0, 32, 32),
            document: Some(RECT.to_string()),
        };
        // (32, 32) is inside the raster but has no ink in the mask.
        let blank = icon(8, (32, 32), Some(RECT));
        let icons = vec![icon(1, (0, 0), Some(RECT)), off, blank];
        let review = host_review(&sheet, &mask, &icons, &options()).expect("a pass");
        assert_eq!(review.icons.len(), 1);
        assert_eq!(review.icons[0].id, id(1));
        let reasons: Vec<(&[u8; 16], &str)> = review
            .skipped
            .iter()
            .map(|skip| (&skip.id, skip.reason.as_str()))
            .collect();
        assert_eq!(reasons.len(), 2);
        assert!(reasons[0].1.contains("off the 64×64 sheet"), "{reasons:?}");
        assert!(reasons[1].1.contains("no ink"), "{reasons:?}");
    }

    #[test]
    fn an_empty_box_is_skipped_without_moving_the_others() {
        // The library can hold a degenerate row; it must not shift the ids.
        let boxes = [boxed(0, 0), boxed(32, 0)];
        let (sheet, mask) = sheet_and_mask(64, &boxes);
        let degenerate = HostIcon {
            id: id(7),
            bbox: (0, 0, 0, 0),
            document: Some(RECT.to_string()),
        };
        let icons = vec![
            icon(1, (0, 0), Some(RECT)),
            degenerate,
            icon(2, (32, 0), Some(SMALLER)),
        ];
        let review = host_review(&sheet, &mask, &icons, &options()).expect("a pass");
        assert_eq!(review.skipped.len(), 1);
        assert_eq!(review.skipped[0].id, id(7));
        assert!(
            review.skipped[0].reason.contains("is empty"),
            "{:?}",
            review.skipped
        );
        let indices: Vec<u32> = review.icons.iter().map(|entry| entry.index).collect();
        assert_eq!(indices, vec![0, 2]);
    }

    #[test]
    fn an_empty_pass_reviews_nothing_and_says_why() {
        let (sheet, mask) = sheet_and_mask(64, &[]);
        let review = host_review(&sheet, &mask, &[], &options()).expect("a pass");
        assert_eq!(review, HostReview::empty(Vec::new()));
    }

    #[test]
    fn a_decision_names_the_library_state_it_puts_an_icon_in() {
        assert_eq!(
            review_state_for(TriageAction::Approve),
            ReviewState::Approved
        );
        assert_eq!(
            review_state_for(TriageAction::BulkApprove),
            ReviewState::Approved
        );
        assert_eq!(
            review_state_for(TriageAction::Reject),
            ReviewState::Rejected
        );
        assert_eq!(review_state_for(TriageAction::Flag), ReviewState::Flagged);
        assert_eq!(
            review_state_for(TriageAction::Duplicate),
            ReviewState::Duplicate
        );
        // Undo restores what was there before — and a first decision leaves
        // nothing behind but `pending`.
        assert_eq!(restored_review_state(None), ReviewState::Pending);
        let mut log = TriageLog::new();
        log.apply(0, TriageAction::Flag, 10);
        // The first decision on an icon replaces nothing, so undo leaves it
        // pending; the second one replaces the flag, so undo puts it back.
        let (_, nothing) = log.undo().expect("the log has one decision");
        assert_eq!(nothing.map(|d| d.action), None);
        log.apply(0, TriageAction::Flag, 20);
        log.apply(0, TriageAction::Reject, 30);
        let (_, replaced) = log.undo().expect("the log has two decisions");
        assert_eq!(replaced.map(|d| d.action), Some(TriageAction::Flag));
        assert_eq!(
            restored_review_state(replaced),
            ReviewState::Flagged,
            "undoing a second decision restores the first one's state"
        );
    }
}
