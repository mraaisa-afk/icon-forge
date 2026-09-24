//! The review workspace's host surface: DTOs for the webview, and the triage
//! log's persistence.
//!
//! [`isg_native::review_host`] answers with library ids; `commands.rs` turns that
//! into the JSON the webview reads. Everything in between — the serde shapes, the
//! mapping from a triage decision onto `icons.review_state`, and the journal that
//! makes a session survive a reload — lives here, away from `tauri::State`, so it
//! is unit-testable without an app handle.
//!
//! **The log is persisted as events, not as state.** Every decision appends one
//! row to `review_log` under the sheet's key — `review/apply action=approve
//! index=4 seq=7 at=1758…` — and undoing appends `review/undo at=…`. Replaying
//! those rows in order rebuilds the decisions *and* the undo history, because
//! `TriageLog::apply` and `TriageLog::undo` follow the same sequence as the
//! original session did. Storing the resulting state instead would be smaller and
//! would lose the one thing the workspace promises: `Ctrl+Z` after a reload.
//!
//! A row that *replays* is checked against the sequence number it recorded. The
//! numbers come out of the same event order, so a mismatch means the journal is
//! not the one this sheet wrote — a truncated or hand-edited log — and the honest
//! answer is to say so rather than to serve decisions nobody made.

use std::collections::BTreeMap;

use serde::Serialize;

use isg_native::db::{Library, ReviewState};
use isg_native::pipeline::score::Score;
use isg_native::review::triage::{TriageAction, TriageLog};
use isg_native::review_host::{HostCluster, HostReview};
use isg_native::IsgError;

/// The prefix byte of a review session's key inside `review_log`.
///
/// Sheet ids are 16-byte blake3 prefixes and icon ids are 16-byte digests, so a
/// 17-byte key that starts with `0x52` ('R') cannot collide with either.
const LOG_PREFIX: u8 = 0x52;

/// The journal's event names, which are also what a reader sees in `sqlite3`.
const APPLY_EVENT: &str = "review/apply";
const UNDO_EVENT: &str = "review/undo";

/// The key one sheet's triage journal is filed under.
#[must_use]
pub fn log_key(sheet: &[u8; 16]) -> Vec<u8> {
    let mut key = Vec::with_capacity(17);
    key.push(LOG_PREFIX);
    key.extend_from_slice(sheet);
    key
}

/// A triage action as the webview names it.
///
/// [`TriageActionDto::parse`] accepts both the CSV name (`approve`) and the
/// keyboard key the workspace binds (`A`), because those are the two spellings a
/// frontend naturally has in hand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriageActionDto {
    /// Keep it.
    Approve,
    /// Drop it.
    Reject,
    /// Needs another look later.
    Flag,
    /// A duplicate of its cluster's keeper.
    Duplicate,
    /// Approved in bulk.
    BulkApprove,
}

impl TriageActionDto {
    /// The action behind one of `review.csv`'s names.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        let lowered = name.trim().to_ascii_lowercase();
        match lowered.as_str() {
            "approve" | "approved" | "a" => Some(Self::Approve),
            "reject" | "rejected" | "r" => Some(Self::Reject),
            "flag" | "flagged" | "f" => Some(Self::Flag),
            "duplicate" | "dup" | "d" => Some(Self::Duplicate),
            "bulk-approve" | "bulk_approve" | "bulk" | "shift+a" => Some(Self::BulkApprove),
            _ => None,
        }
    }

    /// The library action it means.
    #[must_use]
    pub const fn action(self) -> TriageAction {
        match self {
            Self::Approve => TriageAction::Approve,
            Self::Reject => TriageAction::Reject,
            Self::Flag => TriageAction::Flag,
            Self::Duplicate => TriageAction::Duplicate,
            Self::BulkApprove => TriageAction::BulkApprove,
        }
    }

    /// The keyboard shortcut the review workspace binds (§3.6): `A`, `R`, `F`,
    /// `D`, `Shift+A`.
    #[must_use]
    pub const fn key(self) -> &'static str {
        self.action().key()
    }

    /// The canonical name, as `review.csv` writes it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        self.action().as_str()
    }
}

/// One score, as the webview reads it (`Score` has no serde of its own).
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoreOut {
    /// Mean absolute ink-plane error.
    pub mae: f32,
    /// Block SSIM.
    pub ssim: f32,
    /// Ink IoU after centroid alignment.
    pub iou: f32,
    /// The composite the quality flags are read from.
    pub composite: f32,
}

impl From<Score> for ScoreOut {
    fn from(score: Score) -> Self {
        Self {
            mae: score.mae,
            ssim: score.ssim,
            iou: score.iou,
            composite: score.composite,
        }
    }
}

/// The per-icon numbers the outlier detector read.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatOut {
    /// `√ink area` in pixels.
    pub ink_size: f32,
    /// Stroke weight, in pixels.
    pub stroke: f32,
    /// Traced node count.
    pub node_count: f32,
    /// Distinct colours in the traced palette.
    pub colours: f32,
    /// Solidity in `[0, 1]`.
    pub solidity: f32,
    /// Ink area over box area.
    pub fill_ratio: f32,
    /// The folded palette hash, as 16-char lowercase hex.
    ///
    /// A hash is an identity, not a quantity, and JSON numbers stop being exact
    /// in the webview at 2⁵³ — a `u64` would reach the workspace already rounded,
    /// so two palettes that differ in their low bits could compare equal. Hex
    /// text has no such edge.
    pub palette: String,
}

/// One reviewed icon.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IconReviewOut {
    /// 32-char hex of the icon's 16-byte id.
    pub id: String,
    /// The sheet row the icon occupies — the key the triage log uses.
    pub index: u32,
    /// Stage ⑧ score of the document against the sheet's own crop.
    pub score: ScoreOut,
    /// Quality flags, in a fixed order.
    pub flags: Vec<String>,
    /// Segments in the traced outline.
    pub node_count: u32,
    /// True when every subpath is closed.
    pub closed: bool,
    /// Distinct colours in the artwork.
    pub colours: u32,
    /// Ink area in pixels.
    pub ink_area: u64,
    /// The numbers the outlier detector read.
    pub stat: StatOut,
    /// dHash of the normalised cell, as 16-char lowercase hex (see
    /// [`StatOut::palette`] for why these are text).
    pub d_hash: String,
    /// aHash of the normalised cell, as 16-char lowercase hex.
    pub a_hash: String,
    /// 64-char hex of the cell's blake3 digest.
    pub digest: String,
    /// The triage decision so far: a triage action name, or `pending`.
    pub state: String,
    /// The icon's duplicate cluster, when it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cluster: Option<ClusterOut>,
    /// True when this icon is its cluster's suggested keeper.
    pub keeper: bool,
    /// The deviations that put this icon on the outlier list.
    pub outliers: Vec<OutlierOut>,
}

/// A duplicate cluster.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClusterOut {
    /// Member ids (32-char hex), ascending.
    pub members: Vec<String>,
    /// The suggested keeper's id.
    pub keeper: String,
    /// True when every member holds the same bytes — a copy rather than a
    /// variant. The cascade cannot tell a reviewer which of the two it found,
    /// and a UI that shows a "variant" pair as identical artwork would be
    /// asserting something the detectors never claimed.
    pub identical: bool,
}

/// One deviation of one icon, as the sheet-level list reports it.
///
/// The per-icon lists inside [`IconReviewOut::outliers`] need no id — they are
/// already attached to the icon that carries them — but this flat list is what
/// answers "how many deviations does this sheet have, and where", and a list a
/// reader cannot attribute is not answerable. Hence the id.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SheetOutlierOut {
    /// 32-char hex of the icon the deviation belongs to.
    pub icon: String,
    /// The deviation itself.
    #[serde(flatten)]
    pub flag: OutlierOut,
}

/// One deviation.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutlierOut {
    /// The metric's name (`ink-size`, `stroke`, …).
    pub kind: String,
    /// The modified z-score (`0.0` for the categorical kinds).
    pub z: f32,
    /// The icon's value.
    pub value: f32,
    /// The sheet's median for that metric.
    pub median: f32,
}

/// An icon the pass could not review.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedOut {
    /// 32-char hex of the icon's id.
    pub id: String,
    /// Why it was left out.
    pub reason: String,
}

/// The cascade's funnel, so the UI can show what the stages cost.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CascadeOut {
    /// Pairs that shared a band.
    pub candidates: usize,
    /// Candidates that passed verification.
    pub verified: usize,
    /// Verified pairs that were confirmed.
    pub confirmed: usize,
}

/// One triage decision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionOut {
    /// The sheet row the decision names.
    pub index: u32,
    /// 32-char hex of the icon's id.
    pub icon: String,
    /// The action's name.
    pub action: String,
    /// The session's sequence number.
    pub seq: u64,
    /// Unix milliseconds.
    pub at_ms: u64,
}

/// The triage log, as the webview reads it.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TriageStateOut {
    /// How many icons carry a decision.
    pub decided: u32,
    /// The next sequence number (never reused).
    pub seq: u64,
    /// Decisions per action, indexed like `TriageAction::ALL`.
    pub counts: [usize; 5],
    /// True when there is something to undo.
    pub can_undo: bool,
    /// The most recent decision.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last: Option<DecisionOut>,
    /// The size of the export this state would produce.
    pub csv_bytes: usize,
}

/// What an undo did.
///
/// The restored state cannot be read back out of [`TriageStateOut`]: its `last`
/// holds the highest sequence number still in the log, which after an undo may
/// belong to another icon entirely. An undo that named only *which* icon changed
/// would leave the workspace unable to redraw that one row — and the row it
/// cannot redraw is exactly the one it just took back.
#[derive(Clone, Debug, PartialEq)]
pub struct UndoOutcome {
    /// The log's state after the undo.
    pub triage: TriageStateOut,
    /// The icon whose decision was taken back.
    pub icon: [u8; 16],
    /// The library state the icon went back to (`pending` when it had no
    /// earlier decision to restore).
    pub restored: ReviewState,
}

/// One sheet's review, ready for the webview.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewOut {
    /// 32-char hex of the sheet's id.
    pub sheet: String,
    /// Every reviewed icon, in the sheet's row order.
    pub icons: Vec<IconReviewOut>,
    /// Duplicate clusters.
    pub clusters: Vec<ClusterOut>,
    /// The deviations that made the outlier list, each naming its icon.
    pub outliers: Vec<SheetOutlierOut>,
    /// Every icon the pass left out, with the reason.
    pub skipped: Vec<SkippedOut>,
    /// How many icons carry at least one quality flag.
    pub flagged: usize,
    /// The cascade's funnel.
    pub cascade: CascadeOut,
    /// Render + hash time for the whole sheet, in milliseconds.
    pub render_ms: f64,
    /// Cascade + outlier time, in milliseconds.
    pub detect_ms: f64,
    /// The triage state after the pass.
    pub triage: TriageStateOut,
}

/// A review session: which sheet, which icons in which order, and the log.
///
/// The icon list is the sheet's rows in the order the library returns them, which
/// is the order the detectors' `u32` ids index. Keeping it here is what lets a
/// decision name an icon id rather than a position — the frontend never has to
/// know that positions exist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewSession {
    /// The sheet this session belongs to.
    pub sheet: [u8; 16],
    /// Every icon row of the sheet, in row order.
    pub icons: Vec<[u8; 16]>,
    /// The decisions.
    pub log: TriageLog,
}

impl ReviewSession {
    /// A session with no decisions yet.
    #[must_use]
    pub fn new(sheet: [u8; 16], icons: Vec<[u8; 16]>) -> Self {
        Self {
            sheet,
            icons,
            log: TriageLog::new(),
        }
    }

    /// The row an icon occupies, if it belongs to this sheet.
    #[must_use]
    pub fn index_of(&self, icon: &[u8; 16]) -> Option<u32> {
        self.icons
            .iter()
            .position(|candidate| candidate == icon)
            .map(|position| position as u32)
    }

    /// The icon on one row, if the row exists.
    #[must_use]
    pub fn id_of(&self, index: u32) -> Option<[u8; 16]> {
        self.icons.get(index as usize).copied()
    }

    /// The log as the webview reads it.
    #[must_use]
    pub fn state(&self) -> TriageStateOut {
        let csv = self.log.to_csv();
        let last = self
            .log
            .decisions()
            .max_by_key(|(_, decision)| decision.seq)
            .map(|(index, decision)| DecisionOut {
                index,
                icon: hex32(self.id_of(index).unwrap_or([0u8; 16])),
                action: decision.action.as_str().to_string(),
                seq: decision.seq,
                at_ms: decision.at_ms,
            });
        TriageStateOut {
            decided: self.log.len() as u32,
            seq: self.log.next_seq(),
            counts: self.log.counts(),
            can_undo: self.log.can_undo(),
            last,
            csv_bytes: csv.len(),
        }
    }
}

/// Lowercase hex of any byte slice.
#[must_use]
fn hex_of(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// 32-char lowercase hex of a 16-byte id.
#[must_use]
pub fn hex32(id: [u8; 16]) -> String {
    hex_of(&id)
}

/// 64-char lowercase hex of a 32-byte cell digest.
#[must_use]
pub fn hex64(digest: [u8; 32]) -> String {
    hex_of(&digest)
}

/// 16-char lowercase hex of a 64-bit hash.
///
/// Distinct from [`hex64`]: that one renders a 32-*byte* digest, this one a
/// single `u64` — the two names are a byte count either way.
#[must_use]
pub fn hex64_num(value: u64) -> String {
    format!("{value:016x}")
}

/// 32-char hex back to bytes.
#[must_use]
pub fn parse_hex32(text: &str) -> Option<[u8; 16]> {
    let text = text.trim();
    if text.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (index, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(text.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// The exported `review.csv` and the decisions behind it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewExport {
    /// The CSV text (RFC 4180 line endings).
    pub csv: String,
    /// How many decisions it holds.
    pub decisions: u32,
    /// The underlying log's next sequence number.
    pub seq: u64,
}

/// Renders one session's `review.csv`.
#[must_use]
pub fn export(session: &ReviewSession) -> ReviewExport {
    ReviewExport {
        csv: session.log.to_csv(),
        decisions: session.log.len() as u32,
        seq: session.log.next_seq(),
    }
}

/// The journal events one session's decisions leave behind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogEvent {
    /// One decision.
    Apply {
        /// The action's name.
        action: TriageAction,
        /// The sheet row it names.
        index: u32,
        /// The sequence number the session gave it.
        seq: u64,
        /// Unix milliseconds.
        at_ms: u64,
    },
    /// One undo.
    Undo {
        /// Unix milliseconds.
        at_ms: u64,
    },
}

impl LogEvent {
    /// The journal line this event is stored as.
    #[must_use]
    pub fn encode(self) -> String {
        match self {
            Self::Apply {
                action,
                index,
                seq,
                at_ms,
            } => format!(
                "{APPLY_EVENT} action={} index={index} seq={seq} at={at_ms}",
                action.as_str()
            ),
            Self::Undo { at_ms } => format!("{UNDO_EVENT} at={at_ms}"),
        }
    }

    /// Parses a journal line, or `None` when it is not one of ours (the
    /// per-icon audit rows in the same table are not).
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        let line = line.trim();
        let fields: BTreeMap<&str, &str> = line
            .split_once(' ')
            .map(|(_, rest)| rest)
            .unwrap_or("")
            .split_whitespace()
            .filter_map(|field| field.split_once('='))
            .collect();
        let at_ms = fields.get("at").and_then(|v| v.parse().ok());
        if line.starts_with(UNDO_EVENT) {
            return Some(Self::Undo { at_ms: at_ms? });
        }
        if !line.starts_with(APPLY_EVENT) {
            return None;
        }
        let action = TriageAction::parse(fields.get("action")?)?;
        Some(Self::Apply {
            action,
            index: fields.get("index")?.parse().ok()?,
            seq: fields.get("seq")?.parse().ok()?,
            at_ms: at_ms?,
        })
    }
}

/// Replays a sheet's journal into a log.
///
/// Returns the log and how many journal rows it consumed. Every applied decision
/// is checked against the sequence number its row recorded — see the module note.
pub fn replay(rows: &[isg_native::db::ReviewLogRow]) -> Result<TriageLog, IsgError> {
    let mut log = TriageLog::new();
    for row in rows {
        let Some(event) = LogEvent::parse(&row.action) else {
            continue; // a per-icon audit row, not a journal event
        };
        match event {
            LogEvent::Apply {
                action,
                index,
                seq,
                at_ms,
            } => {
                let recorded = log.apply(index, action, at_ms);
                if recorded != seq {
                    return Err(IsgError::Corrupt(format!(
                        "review log replay produced seq {recorded} where the journal recorded {seq}"
                    )));
                }
            }
            LogEvent::Undo { .. } => {
                if log.undo().is_none() {
                    return Err(IsgError::Corrupt(
                        "review log replay hit an undo with nothing to undo".to_string(),
                    ));
                }
            }
        }
    }
    Ok(log)
}

/// Loads one sheet's session, replaying whatever journal it has.
pub fn session_for(
    sheet: [u8; 16],
    icons: Vec<[u8; 16]>,
    lib: &Library,
) -> Result<ReviewSession, IsgError> {
    let rows = lib.review_log_for(&log_key(&sheet))?;
    let log = replay(&rows)?;
    Ok(ReviewSession { sheet, icons, log })
}

/// Records one decision: the log, the icon's `review_state`, and the journal row.
///
/// All three move together or none does — the caller holds the library lock, and
/// a failure after the log was updated is reported rather than swallowed, because
/// the in-memory log is rebuilt from the journal on the next load either way.
pub fn apply_decision(
    session: &mut ReviewSession,
    lib: &mut Library,
    icon: [u8; 16],
    action: TriageActionDto,
) -> Result<TriageStateOut, IsgError> {
    let Some(index) = session.index_of(&icon) else {
        return Err(IsgError::Corrupt(format!(
            "icon {} is not part of sheet {}",
            hex32(icon),
            hex32(session.sheet)
        )));
    };
    let at_ms = now_millis();
    let seq = session.log.apply(index, action.action(), at_ms);
    lib.set_review_state(icon, state_for(action.action()), action.name())?;
    lib.append_review_event(
        &log_key(&session.sheet),
        &LogEvent::Apply {
            action: action.action(),
            index,
            seq,
            at_ms,
        }
        .encode(),
    )?;
    Ok(session.state())
}

/// Undoes the most recent decision, in the log and in the library.
///
/// Returns the new state, the icon that changed and the state it was put back
/// into, or `None` when there was nothing to undo.
pub fn undo_decision(
    session: &mut ReviewSession,
    lib: &mut Library,
) -> Result<Option<UndoOutcome>, IsgError> {
    let Some((index, previous)) = session.log.undo() else {
        return Ok(None);
    };
    let Some(icon) = session.id_of(index) else {
        return Err(IsgError::Corrupt(format!(
            "the triage log holds row {index}, which this sheet does not have"
        )));
    };
    let at_ms = now_millis();
    let restored = restored_action(previous);
    lib.set_review_state(icon, restored, "undo")?;
    lib.append_review_event(&log_key(&session.sheet), &LogEvent::Undo { at_ms }.encode())?;
    Ok(Some(UndoOutcome {
        triage: session.state(),
        icon,
        restored,
    }))
}

/// The library state a decision puts an icon in — one definition, shared with
/// [`isg_native::review_host`].
#[must_use]
pub fn state_for(action: TriageAction) -> ReviewState {
    isg_native::review_host::review_state_for(action)
}

/// The state an undone decision restores.
#[must_use]
pub fn restored_action(
    previous: Option<isg_native::review::triage::TriageDecision>,
) -> ReviewState {
    isg_native::review_host::restored_review_state(previous)
}

/// Unix milliseconds.
#[must_use]
pub fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The webview's view of a pass.
///
/// `session` supplies the triage decisions: the report says what the detectors
/// found, the session says what a human has already decided about it.
#[must_use]
pub fn review_out(report: &HostReview, session: &ReviewSession) -> ReviewOut {
    let clusters: Vec<ClusterOut> = report
        .clusters
        .iter()
        .map(|cluster| ClusterOut {
            members: cluster.members.iter().map(|id| hex32(*id)).collect(),
            keeper: hex32(cluster.keeper),
            identical: identical_cluster(cluster, report),
        })
        .collect();
    let cluster_of: BTreeMap<[u8; 16], usize> = report
        .clusters
        .iter()
        .enumerate()
        .flat_map(|(position, cluster)| cluster.members.iter().map(move |id| (*id, position)))
        .collect();
    let outliers_of: BTreeMap<[u8; 16], Vec<OutlierOut>> = {
        let mut map: BTreeMap<[u8; 16], Vec<OutlierOut>> = BTreeMap::new();
        for outlier in &report.outliers {
            map.entry(outlier.id)
                .or_default()
                .push(outlier_out(outlier.flag));
        }
        map
    };

    let icons = report
        .icons
        .iter()
        .map(|entry| {
            let review = &entry.review;
            IconReviewOut {
                id: hex32(entry.id),
                index: entry.index,
                score: review.score.into(),
                flags: review
                    .flags
                    .iter()
                    .map(|flag| flag.as_str().to_string())
                    .collect(),
                node_count: review.node_count,
                closed: review.closed,
                colours: review.colours,
                ink_area: review.ink_area,
                stat: StatOut {
                    ink_size: review.stat.ink_size,
                    stroke: review.stat.stroke,
                    node_count: review.stat.node_count,
                    colours: review.stat.colours,
                    solidity: review.stat.solidity,
                    fill_ratio: review.stat.fill_ratio,
                    palette: hex64_num(review.stat.palette),
                },
                d_hash: hex64_num(review.d_hash),
                a_hash: hex64_num(review.a_hash),
                digest: hex64(entry.review.digest),
                state: state_name(session, entry.index),
                cluster: cluster_of
                    .get(&entry.id)
                    .map(|position| clusters[*position].clone()),
                keeper: cluster_of
                    .get(&entry.id)
                    .is_some_and(|position| report.clusters[*position].keeper == entry.id),
                outliers: outliers_of.get(&entry.id).cloned().unwrap_or_default(),
            }
        })
        .collect();

    ReviewOut {
        sheet: hex32(session.sheet),
        icons,
        clusters,
        outliers: report
            .outliers
            .iter()
            .map(|outlier| SheetOutlierOut {
                icon: hex32(outlier.id),
                flag: outlier_out(outlier.flag),
            })
            .collect(),
        skipped: report
            .skipped
            .iter()
            .map(|skip| SkippedOut {
                id: hex32(skip.id),
                reason: skip.reason.clone(),
            })
            .collect(),
        flagged: report.flagged,
        cascade: CascadeOut {
            candidates: report.cascade.candidates,
            verified: report.cascade.verified,
            confirmed: report.cascade.confirmed,
        },
        render_ms: report.render_ms,
        detect_ms: report.detect_ms,
        triage: session.state(),
    }
}

/// The deviations of one outlier row.
fn outlier_out(flag: isg_native::review::OutlierFlag) -> OutlierOut {
    OutlierOut {
        kind: flag.kind.as_str().to_string(),
        z: flag.z,
        value: flag.value,
        median: flag.median,
    }
}

/// The triage state of one row, by its index.
fn state_name(session: &ReviewSession, index: u32) -> String {
    session
        .log
        .decision(index)
        .map(|decision| decision.action.as_str().to_string())
        .unwrap_or_else(|| ReviewState::Pending.as_str().to_string())
}

/// True when every member of a cluster carries the same cell digest.
///
/// The cascade reports pairs that are *close*, and §3.6's confirm stage accepts a
/// digest match or an SSIM match — so a cluster can be a set of copies or a set of
/// variants. A reviewer deciding "keep this one" needs to know which, and the
/// report is the only place that knows.
fn identical_cluster(cluster: &HostCluster, report: &HostReview) -> bool {
    let mut digests = cluster.members.iter().map(|id| {
        report
            .icons
            .iter()
            .find(|entry| entry.id == *id)
            .map(|entry| entry.review.digest)
    });
    match digests.next() {
        Some(Some(first)) => digests.all(|digest| digest == Some(first)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use isg_native::review::{IconStat, OutlierFlag, OutlierKind, QualityFlag, TriageDecision};
    use isg_native::review_host::{HostCluster, HostIconReview, HostOutlier, HostSkip};
    use isg_native::review_native::{CascadeCounts, IconReview};

    /// A sheet id that is not a run of zeros, so a mix-up with an icon id shows.
    fn sheet_id() -> [u8; 16] {
        let mut id = [0xA1u8; 16];
        id[15] = 0x5C;
        id
    }

    /// The icon on row `n` of the test sheet.
    fn icon(n: u8) -> [u8; 16] {
        let mut id = [0u8; 16];
        id[0] = n;
        id[15] = n.wrapping_mul(11);
        id
    }

    fn sheet_icons() -> Vec<[u8; 16]> {
        (1..=4).map(icon).collect()
    }

    /// A library with no journal and the session that reads it.
    fn fresh() -> (Library, ReviewSession) {
        let mut lib = Library::open_in_memory().expect("an in-memory library");
        let session = session_for(sheet_id(), sheet_icons(), &mut lib).expect("an empty journal");
        (lib, session)
    }

    fn decide(
        session: &mut ReviewSession,
        lib: &mut Library,
        n: u8,
        action: TriageActionDto,
    ) -> TriageStateOut {
        apply_decision(session, lib, icon(n), action).expect("a decision on a row of this sheet")
    }

    #[test]
    fn a_journal_line_round_trips() {
        for action in TriageAction::ALL {
            let event = LogEvent::Apply {
                action,
                index: 7,
                seq: 3,
                at_ms: 1_700_000_000_000,
            };
            assert_eq!(
                LogEvent::parse(&event.encode()),
                Some(event),
                "{}",
                event.encode()
            );
        }
        let undo = LogEvent::Undo { at_ms: 42 };
        assert_eq!(LogEvent::parse(&undo.encode()), Some(undo));
        // The per-icon audit rows live in the same table and are not events —
        // `replay` skips them, so a session is never rebuilt from them.
        for line in ["approved", "rejected", "review", "review/", ""] {
            assert_eq!(LogEvent::parse(line), None, "{line:?}");
        }
        // An event missing a field is not guessed at.
        for line in [
            "review/apply action=approve index=1",
            "review/apply index=1 seq=0 at=2",
            "review/apply action=approve index=1 seq=0",
            "review/apply action=maybe index=1 seq=0 at=2",
            "review/undo",
        ] {
            assert_eq!(LogEvent::parse(line), None, "{line:?}");
        }
    }

    #[test]
    fn a_journal_line_names_its_action_the_way_the_csv_does() {
        let event = LogEvent::Apply {
            action: TriageAction::BulkApprove,
            index: 0,
            seq: 0,
            at_ms: 1,
        };
        assert_eq!(
            event.encode(),
            "review/apply action=bulk-approve index=0 seq=0 at=1"
        );
        assert_eq!(
            export(&ReviewSession::new(sheet_id(), vec![])).csv,
            "seq,id,action,at_ms\r\n"
        );
    }

    #[test]
    fn replay_rebuilds_the_log_and_refuses_a_jumped_journal() {
        let row = |action: &str| isg_native::db::ReviewLogRow {
            icon_id: [0u8; 16],
            action: action.to_string(),
            timestamp: "0".to_string(),
        };
        let rows = vec![
            row("review/apply action=approve index=0 seq=0 at=10"),
            row("review/apply action=reject index=2 seq=1 at=11"),
            row("review/undo at=12"),
        ];
        let log = replay(&rows).expect("a journal this sheet could have written");
        assert_eq!(log.next_seq(), 2);
        assert_eq!(log.len(), 1);
        assert_eq!(
            log.decision(0).map(|d| d.action),
            Some(TriageAction::Approve)
        );
        assert_eq!(log.decision(2), None, "the undo took the reject back");

        // A sequence number that is not the one the log hands out means these
        // rows are not this session's history.
        let jumped = vec![
            row("review/apply action=approve index=0 seq=0 at=10"),
            row("review/apply action=flag index=1 seq=9 at=11"),
        ];
        assert!(matches!(replay(&jumped), Err(IsgError::Corrupt(_))));
        let repeated = vec![
            row("review/apply action=approve index=0 seq=0 at=10"),
            row("review/apply action=flag index=1 seq=0 at=11"),
        ];
        assert!(matches!(replay(&repeated), Err(IsgError::Corrupt(_))));
        // An undo with nothing behind it is the same kind of damage.
        assert!(matches!(
            replay(&[row("review/undo at=10")]),
            Err(IsgError::Corrupt(_))
        ));
    }

    #[test]
    fn a_decision_survives_a_reload_with_its_undo_stack() {
        let (mut lib, mut live) = fresh();
        decide(&mut live, &mut lib, 1, TriageActionDto::Approve);
        decide(&mut live, &mut lib, 2, TriageActionDto::Reject);
        decide(&mut live, &mut lib, 3, TriageActionDto::Flag);
        let outcome = undo_decision(&mut live, &mut lib)
            .expect("an undo")
            .expect("something to undo");
        let (state, changed) = (outcome.triage.clone(), outcome.icon);
        assert_eq!(changed, icon(3));
        assert_eq!(state.decided, 2);
        assert_eq!(state.counts, [1, 1, 0, 0, 0]);
        assert_eq!(state.last.as_ref().map(|d| d.index), Some(1));
        assert!(
            state.can_undo,
            "the first two decisions are still there to undo"
        );

        // A fresh session over the same rows replays the journal, and lands on
        // the same log the live session holds.
        let reloaded = session_for(sheet_id(), sheet_icons(), &mut lib).expect("the journal");
        assert_eq!(
            reloaded.log, live.log,
            "the replay is the log the user made"
        );
        assert_eq!(
            reloaded.log.next_seq(),
            3,
            "sequence numbers do not go backwards"
        );
        assert_eq!(reloaded.state(), live.state());
        assert_eq!(
            reloaded.log.decision(0).map(|d| d.action),
            Some(TriageAction::Approve)
        );
        assert_eq!(
            reloaded.log.decision(1).map(|d| d.action),
            Some(TriageAction::Reject)
        );
        assert_eq!(reloaded.log.decision(2), None, "icon 3 was undone");

        // And the reloaded session can carry on: the next decision takes the
        // sequence number the journal is due, not a fresh 0.
        let mut reloaded = reloaded;
        let state = decide(&mut reloaded, &mut lib, 4, TriageActionDto::Duplicate);
        assert_eq!(state.seq, 4);
        assert_eq!(state.counts, [1, 1, 0, 1, 0]);
        let again = session_for(sheet_id(), sheet_icons(), &mut lib).expect("the journal");
        assert_eq!(again.log, reloaded.log);
    }

    #[test]
    fn an_undo_replays_without_its_own_sequence_number() {
        let (mut lib, mut live) = fresh();
        decide(&mut live, &mut lib, 1, TriageActionDto::Approve);
        assert!(undo_decision(&mut live, &mut lib).expect("undo").is_some());
        let reloaded = session_for(sheet_id(), sheet_icons(), &mut lib).expect("the journal");
        assert!(reloaded.log.is_empty(), "the only decision was undone");
        assert_eq!(reloaded.log.next_seq(), 1, "the sequence number was spent");
        assert!(!reloaded.log.can_undo(), "the undo stack is empty again");
        assert!(!reloaded.state().can_undo);
        assert_eq!(reloaded.state().decided, 0);
        assert_eq!(reloaded.state().last, None);
    }

    #[test]
    fn a_second_decision_on_one_icon_undoes_back_to_the_first() {
        let (mut lib, mut live) = fresh();
        decide(&mut live, &mut lib, 2, TriageActionDto::Approve);
        decide(&mut live, &mut lib, 2, TriageActionDto::Reject);
        assert_eq!(live.log.counts(), [0, 1, 0, 0, 0]);
        let outcome = undo_decision(&mut live, &mut lib)
            .expect("undo")
            .expect("history");
        let (state, changed) = (outcome.triage.clone(), outcome.icon);
        assert_eq!(changed, icon(2));
        assert_eq!(state.counts, [1, 0, 0, 0, 0], "the approve came back");
        assert_eq!(
            outcome.restored,
            ReviewState::Approved,
            "the undo says what the icon went back to"
        );
        // The library agrees with the log: the undo restored the earlier state.
        assert_eq!(
            restored_action(Some(TriageDecision {
                action: TriageAction::Approve,
                seq: 0,
                at_ms: 0,
            })),
            ReviewState::Approved
        );

        let reloaded = session_for(sheet_id(), sheet_icons(), &mut lib).expect("the journal");
        assert_eq!(reloaded.log, live.log);
        assert_eq!(
            reloaded.log.decision(1).map(|d| d.action),
            Some(TriageAction::Approve)
        );
        assert_eq!(reloaded.log.next_seq(), 2);
    }

    #[test]
    fn a_decision_for_an_icon_outside_the_sheet_is_refused() {
        let (mut lib, mut live) = fresh();
        let error = apply_decision(&mut live, &mut lib, icon(9), TriageActionDto::Approve)
            .expect_err("icon 9 is not a row of this sheet");
        assert!(matches!(error, IsgError::Corrupt(_)), "{error:?}");
        assert!(live.log.is_empty(), "nothing was recorded");
        let rows = lib
            .review_log_for(&log_key(&sheet_id()))
            .expect("the journal");
        assert!(rows.is_empty(), "and nothing reached the table");
        // An undo on an untouched log is a no-op rather than an error.
        assert_eq!(undo_decision(&mut live, &mut lib).expect("undo"), None);
    }

    #[test]
    fn a_decision_left_on_a_row_the_sheet_lost_is_reported() {
        let (mut lib, mut live) = fresh();
        decide(&mut live, &mut lib, 4, TriageActionDto::Approve);
        // The library came back with fewer rows than the journal was written
        // against — undoing a decision for an icon that is no longer there is
        // damage, not a silent skip.
        live.icons.truncate(2);
        assert!(matches!(
            undo_decision(&mut live, &mut lib),
            Err(IsgError::Corrupt(_))
        ));
    }

    #[test]
    fn a_decision_moves_the_state_the_review_host_defines() {
        for (action, state) in [
            (TriageAction::Approve, ReviewState::Approved),
            (TriageAction::BulkApprove, ReviewState::Approved),
            (TriageAction::Reject, ReviewState::Rejected),
            (TriageAction::Flag, ReviewState::Flagged),
            (TriageAction::Duplicate, ReviewState::Duplicate),
        ] {
            assert_eq!(state_for(action), state);
        }
        assert_eq!(restored_action(None), ReviewState::Pending);
        assert_eq!(export(&ReviewSession::new(sheet_id(), vec![])).decisions, 0);
    }

    #[test]
    fn the_export_is_the_logs_own_csv() {
        let (mut lib, mut live) = fresh();
        decide(&mut live, &mut lib, 2, TriageActionDto::Reject);
        let export = export(&live);
        assert_eq!(export.decisions, 1);
        assert_eq!(export.seq, 1);
        assert_eq!(export.csv, live.log.to_csv());
        assert!(
            export.csv.starts_with("seq,id,action,at_ms"),
            "{}",
            export.csv
        );
        // The file names the row the decision was made on, which is the key the
        // triage log and the detectors share — icon 2 is row 1.
        let row: Vec<&str> = export
            .csv
            .lines()
            .nth(1)
            .expect("one decision")
            .split(',')
            .collect();
        assert_eq!(&row[..3], ["0", "1", "reject"], "{}", export.csv);
        assert_eq!(live.state().csv_bytes, export.csv.len());
        // What the file holds is the decisions, not the undo stack: a reader
        // gets the same decisions back and nothing to undo — the history lives
        // in the journal, not in the export.
        let reloaded = TriageLog::from_csv(&export.csv).expect("the file reads back");
        assert_eq!(reloaded.len(), live.log.len());
        assert_eq!(reloaded.counts(), live.log.counts());
        assert_eq!(
            reloaded.decision(1).map(|d| d.action),
            Some(TriageAction::Reject)
        );
        assert!(!reloaded.can_undo(), "the export is a record, not a stack");
    }

    #[test]
    fn the_report_names_icons_clusters_and_decisions() {
        let (mut lib, mut live) = fresh();
        decide(&mut live, &mut lib, 2, TriageActionDto::Flag);
        decide(&mut live, &mut lib, 1, TriageActionDto::Approve);

        let review = |id: u32, digest: [u8; 32], flags: Vec<QualityFlag>| IconReview {
            id,
            score: Score {
                mae: 0.01,
                ssim: 0.99,
                iou: 0.98,
                composite: 0.97,
            },
            flags,
            node_count: 12,
            closed: false,
            colours: 2,
            ink_area: 400,
            stat: IconStat {
                id,
                ink_size: 20.0,
                stroke: 3.0,
                node_count: 12.0,
                colours: 2.0,
                solidity: 0.9,
                fill_ratio: 0.7,
                palette: 5,
            },
            d_hash: 1,
            a_hash: 2,
            digest,
        };
        let report = HostReview {
            icons: vec![
                HostIconReview {
                    id: icon(1),
                    index: 0,
                    review: review(0, [7u8; 32], vec![QualityFlag::OpenContour]),
                },
                HostIconReview {
                    id: icon(2),
                    index: 1,
                    review: review(1, [7u8; 32], Vec::new()),
                },
                HostIconReview {
                    id: icon(3),
                    index: 2,
                    review: review(2, [9u8; 32], Vec::new()),
                },
            ],
            skipped: vec![HostSkip {
                id: icon(4),
                reason: "no ink in its box".to_string(),
            }],
            clusters: vec![
                HostCluster {
                    members: vec![icon(1), icon(2)],
                    keeper: icon(2),
                },
                HostCluster {
                    members: vec![icon(3), icon(4)],
                    keeper: icon(3),
                },
            ],
            outliers: vec![HostOutlier {
                id: icon(3),
                flag: OutlierFlag {
                    id: 2,
                    kind: OutlierKind::Stroke,
                    z: 4.5,
                    value: 9.0,
                    median: 3.0,
                },
            }],
            flagged: 1,
            cascade: CascadeCounts {
                candidates: 6,
                verified: 4,
                confirmed: 2,
            },
            render_ms: 12.5,
            detect_ms: 3.25,
        };

        let out = review_out(&report, &live);
        assert_eq!(out.sheet, hex32(sheet_id()));
        assert_eq!(out.icons.len(), 3);
        assert_eq!(out.flagged, 1);
        assert_eq!(out.render_ms, 12.5);
        assert_eq!(out.detect_ms, 3.25);
        assert_eq!(
            out.cascade,
            CascadeOut {
                candidates: 6,
                verified: 4,
                confirmed: 2,
            }
        );
        assert_eq!(out.outliers.len(), 1);
        assert_eq!(
            out.outliers[0].icon,
            hex32(icon(3)),
            "a sheet-level deviation names the icon it is about"
        );
        assert_eq!(out.outliers[0].flag.kind, "stroke");
        assert_eq!(out.skipped.len(), 1);
        assert_eq!(out.skipped[0].id, hex32(icon(4)));
        assert_eq!(out.skipped[0].reason, "no ink in its box");

        // Clusters are in library ids, each member carries its own cluster, and
        // being the keeper is a property of the member, not of the cluster.
        assert_eq!(
            out.clusters[0].members,
            vec![hex32(icon(1)), hex32(icon(2))]
        );
        assert_eq!(out.clusters[0].keeper, hex32(icon(2)));
        assert!(
            out.clusters[0].identical,
            "both members hash to the same cell"
        );
        assert!(!out.clusters[1].identical, "icon 4 has no cell to hash");
        assert_eq!(
            out.icons[0].cluster.as_ref().map(|c| c.keeper.clone()),
            Some(hex32(icon(2)))
        );
        assert!(!out.icons[0].keeper);
        assert!(out.icons[1].keeper);

        // Flags, the outlier and the triage decision travel with their icon.
        assert_eq!(out.icons[0].flags, vec!["open-contour".to_string()]);
        assert_eq!(out.icons[1].flags, Vec::<String>::new());
        assert_eq!(out.icons[2].outliers.len(), 1);
        assert_eq!(out.icons[2].outliers[0].kind, "stroke");
        assert_eq!(out.icons[2].outliers[0].z, 4.5);
        assert_eq!(out.icons[0].digest, hex64([7u8; 32]));
        assert_eq!(out.icons[0].state, "approve");
        assert_eq!(out.icons[1].state, "flag");
        assert_eq!(out.icons[2].state, "pending", "icon 3 has not been decided");
        assert_eq!(out.icons[0].score.composite, 0.97);
        assert_eq!(out.icons[0].stat.ink_size, 20.0);
        // The hashes travel as text: `u64` is not exact in the webview.
        assert_eq!(out.icons[0].d_hash, "0000000000000001");
        assert_eq!(out.icons[0].a_hash, "0000000000000002");
        assert_eq!(out.icons[0].stat.palette, "0000000000000005");
        assert_eq!(
            out.icons[0].d_hash.len(),
            16,
            "every hash is padded to one width, so a UI can align them"
        );
        assert_eq!(out.icons[0].index, 0);

        // The triage state is the session's, and its `last` is the newest
        // decision by sequence number rather than by row.
        assert_eq!(out.triage.decided, 2);
        assert_eq!(out.triage.seq, 2);
        assert_eq!(out.triage.counts, [1, 0, 1, 0, 0]);
        let last = out.triage.last.expect("two decisions were made");
        assert_eq!(last.index, 0);
        assert_eq!(last.icon, hex32(icon(1)));
        assert_eq!(last.action, "approve");
        assert_eq!(last.seq, 1);
    }

    #[test]
    fn an_empty_pass_reports_an_empty_sheet() {
        let (_lib, live) = fresh();
        let out = review_out(
            &HostReview::empty(vec![HostSkip {
                id: icon(1),
                reason: "its row has an empty box".to_string(),
            }]),
            &live,
        );
        assert!(out.icons.is_empty());
        assert!(out.clusters.is_empty());
        assert!(out.outliers.is_empty());
        assert_eq!(out.skipped.len(), 1, "the skips are the report");
        assert_eq!(out.flagged, 0);
        assert_eq!(out.triage.decided, 0);
        assert!(!out.triage.can_undo);
    }

    #[test]
    fn hex_ids_round_trip_and_junk_is_refused() {
        let text = hex32(icon(3));
        assert_eq!(text.len(), 32);
        assert_eq!(parse_hex32(&text), Some(icon(3)));
        assert_eq!(parse_hex32(&text.to_uppercase()), Some(icon(3)));
        assert_eq!(parse_hex32(" ab "), None);
        assert_eq!(parse_hex32("abc"), None);
        assert_eq!(parse_hex32(&"z".repeat(32)), None);
        assert_eq!(parse_hex32(&"0".repeat(33)), None);
        assert_eq!(hex64([0xff; 32]).len(), 64);
        assert_eq!(hex32([0u8; 16]), "0".repeat(32));
        assert_eq!(hex64_num(u64::MAX), "f".repeat(16));
        assert_eq!(hex64_num(0), "0".repeat(16));
    }

    #[test]
    fn actions_parse_from_both_the_csv_name_and_the_key() {
        assert_eq!(
            TriageActionDto::parse("approve"),
            Some(TriageActionDto::Approve)
        );
        assert_eq!(
            TriageActionDto::parse(" A "),
            Some(TriageActionDto::Approve)
        );
        assert_eq!(TriageActionDto::parse("a"), Some(TriageActionDto::Approve));
        assert_eq!(
            TriageActionDto::parse("reject"),
            Some(TriageActionDto::Reject)
        );
        assert_eq!(TriageActionDto::parse("R"), Some(TriageActionDto::Reject));
        assert_eq!(TriageActionDto::parse("flag"), Some(TriageActionDto::Flag));
        assert_eq!(TriageActionDto::parse("F"), Some(TriageActionDto::Flag));
        assert_eq!(
            TriageActionDto::parse("duplicate"),
            Some(TriageActionDto::Duplicate)
        );
        assert_eq!(
            TriageActionDto::parse("d"),
            Some(TriageActionDto::Duplicate)
        );
        assert_eq!(
            TriageActionDto::parse("bulk-approve"),
            Some(TriageActionDto::BulkApprove)
        );
        assert_eq!(
            TriageActionDto::parse("Shift+A"),
            Some(TriageActionDto::BulkApprove)
        );
        assert_eq!(
            TriageActionDto::parse("shift+a"),
            Some(TriageActionDto::BulkApprove)
        );
        assert_eq!(TriageActionDto::parse("maybe"), None);
        assert_eq!(TriageActionDto::parse(""), None);
        // Every action names itself the way the CSV and the journal do, and its
        // name is the one the triage log parses back.
        for action in TriageAction::ALL {
            let dto =
                TriageActionDto::parse(TriageAction::parse(action.as_str()).unwrap().as_str())
                    .expect("the library's name is one the host knows");
            assert_eq!(dto.action(), action);
            assert_eq!(dto.name(), action.as_str());
            assert_eq!(dto.key(), action.key());
        }
    }

    #[test]
    fn a_session_key_cannot_collide_with_a_sheet_or_icon_id() {
        let key = log_key(&sheet_id());
        assert_eq!(key.len(), 17, "one prefix byte plus the sheet id");
        assert_eq!(key[0], 0x52, "`R`");
        assert_eq!(&key[1..], &sheet_id());
        // No 16-byte id can equal it, so the per-icon audit rows of one sheet's
        // icons and that sheet's own journal never share a key.
        assert_ne!(key.len(), 16);
        let other = log_key(&icon(1));
        assert_ne!(key, other);
        assert_eq!(other.len(), 17);
    }
}
