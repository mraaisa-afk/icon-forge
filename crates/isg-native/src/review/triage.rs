//! §3.6 triage — the decision log and `review.csv`.
//!
//! A review session is a sequence of keyboard decisions, and the roadmap asks
//! for three properties of them: every decision is **timestamped**, every
//! decision is **undoable**, and the whole session exports as `review.csv`.
//!
//! The log is therefore not a list of actions but a *state*: at most one
//! decision per icon, plus enough history to undo. Undo restores whatever was
//! there before — an icon that was flagged and then rejected goes back to
//! flagged, not to undecided — which is what makes the UI's `Ctrl+Z` honest.
//!
//! Two smaller decisions worth knowing:
//!
//! * **Sequence numbers never go backwards.** An undo removes a decision but
//!   does not free its number, so `seq` is unique for the lifetime of a
//!   session and two exports of the same session differ only by what was
//!   decided in between.
//! * **An imported log has no history.** Reading `review.csv` back rebuilds
//!   the decisions (that is what makes the export verifiable) but the session
//!   starts a fresh undo stack, because the keystrokes that produced the file
//!   are not in it.

use std::collections::BTreeMap;

use crate::sheet::export::parse_csv;

/// The `review.csv` columns, in order.
pub const REVIEW_COLUMNS: [&str; 4] = ["seq", "id", "action", "at_ms"];

/// The `review.csv` header line (RFC 4180 line ending included by the writer).
pub const REVIEW_HEADER: &str = "seq,id,action,at_ms";

/// What a reviewer can decide about an icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TriageAction {
    /// Keep it (§3.6 `A`).
    Approve,
    /// Drop it (`R`).
    Reject,
    /// Needs another look later (`F`).
    Flag,
    /// Marked as a duplicate of its cluster's keeper (`D`).
    Duplicate,
    /// Approved in bulk, together with everything else selected (`Shift+A`).
    BulkApprove,
}

impl TriageAction {
    /// Every action, in the order the UI shows them.
    pub const ALL: [Self; 5] = [
        Self::Approve,
        Self::Reject,
        Self::Flag,
        Self::Duplicate,
        Self::BulkApprove,
    ];

    /// The stable name used by `review.csv` and the log lines.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Reject => "reject",
            Self::Flag => "flag",
            Self::Duplicate => "duplicate",
            Self::BulkApprove => "bulk-approve",
        }
    }

    /// Parses [`TriageAction::as_str`] back.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "approve" => Some(Self::Approve),
            "reject" => Some(Self::Reject),
            "flag" => Some(Self::Flag),
            "duplicate" => Some(Self::Duplicate),
            "bulk-approve" => Some(Self::BulkApprove),
            _ => None,
        }
    }

    /// The keyboard shortcut the review workspace binds (§3.6).
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Approve => "A",
            Self::Reject => "R",
            Self::Flag => "F",
            Self::Duplicate => "D",
            Self::BulkApprove => "Shift+A",
        }
    }

    /// The index into [`TriageLog::counts`].
    #[must_use]
    const fn index(self) -> usize {
        match self {
            Self::Approve => 0,
            Self::Reject => 1,
            Self::Flag => 2,
            Self::Duplicate => 3,
            Self::BulkApprove => 4,
        }
    }
}

/// One decision, as it goes into the log and the CSV.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TriageDecision {
    /// What was decided.
    pub action: TriageAction,
    /// The session's sequence number for this decision.
    pub seq: u64,
    /// Unix milliseconds at which it was made.
    pub at_ms: u64,
}

/// Why a `review.csv` could not be read back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TriageError {
    /// A row is not a valid decision.
    Malformed {
        /// 1-based line number in the file.
        line: usize,
        /// What was wrong.
        reason: String,
    },
    /// The header is missing a required column.
    MissingColumn(String),
}

impl std::fmt::Display for TriageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed { line, reason } => write!(f, "line {line}: {reason}"),
            Self::MissingColumn(name) => write!(f, "missing column `{name}`"),
        }
    }
}

impl std::error::Error for TriageError {}

/// The session's decisions, with undo.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TriageLog {
    decisions: BTreeMap<u32, TriageDecision>,
    /// `(icon, what was there before)` per applied decision, oldest first.
    history: Vec<(u32, Option<TriageDecision>)>,
    next_seq: u64,
}

impl TriageLog {
    /// An empty log.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a decision, returning its sequence number.
    ///
    /// A second decision on the same icon replaces the first — the log holds
    /// the *current* state, and the replaced one is what [`TriageLog::undo`]
    /// puts back.
    pub fn apply(&mut self, id: u32, action: TriageAction, at_ms: u64) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        let previous = self
            .decisions
            .insert(id, TriageDecision { action, seq, at_ms });
        self.history.push((id, previous));
        seq
    }

    /// Reverts the most recent decision, returning the icon and the decision
    /// that was removed (`None` when the icon is undecided again).
    pub fn undo(&mut self) -> Option<(u32, Option<TriageDecision>)> {
        let (id, previous) = self.history.pop()?;
        match previous {
            Some(decision) => {
                self.decisions.insert(id, decision);
            }
            None => {
                self.decisions.remove(&id);
            }
        }
        Some((id, previous))
    }

    /// The next sequence number this log will hand out.
    ///
    /// Exposed because the number is part of the export's contract: a reader
    /// comparing two `review.csv`s of one session can tell that nothing was lost
    /// from the gap between the highest `seq` and this.
    #[must_use]
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// True when there is a decision left to undo.
    ///
    /// The workspace binds `Ctrl+Z` to [`TriageLog::undo`], and an undo that
    /// returns `None` on an empty history is a keystroke the user should never
    /// have been invited to make — so it asks first.
    #[must_use]
    pub fn can_undo(&self) -> bool {
        !self.history.is_empty()
    }

    /// The current decision for an icon, if any.
    #[must_use]
    pub fn decision(&self, id: u32) -> Option<TriageDecision> {
        self.decisions.get(&id).copied()
    }

    /// How many icons are decided.
    #[must_use]
    pub fn len(&self) -> usize {
        self.decisions.len()
    }

    /// True when nothing has been decided.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.decisions.is_empty()
    }

    /// Decided icons, in ascending id order.
    pub fn decisions(&self) -> impl Iterator<Item = (u32, TriageDecision)> + '_ {
        self.decisions.iter().map(|(id, d)| (*id, *d))
    }

    /// How many decisions per action, indexed like [`TriageAction::ALL`].
    #[must_use]
    pub fn counts(&self) -> [usize; 5] {
        let mut counts = [0usize; 5];
        for decision in self.decisions.values() {
            counts[decision.action.index()] += 1;
        }
        counts
    }

    /// The audit trail as RFC 4180 CSV, rows ordered by sequence number.
    ///
    /// Chronological order, not id order: the file answers "what did the
    /// reviewer do, in what order", and an id's later decision appears after
    /// the one it replaced (only the current one is in the file).
    #[must_use]
    pub fn to_csv(&self) -> String {
        let mut rows: Vec<(u32, TriageDecision)> =
            self.decisions.iter().map(|(id, d)| (*id, *d)).collect();
        rows.sort_by_key(|(_, d)| d.seq);
        let mut out = String::with_capacity(REVIEW_HEADER.len() + rows.len() * 24 + 2);
        out.push_str(REVIEW_HEADER);
        out.push_str("\r\n");
        for (id, decision) in rows {
            let line = format!(
                "{},{},{},{}",
                decision.seq,
                id,
                decision.action.as_str(),
                decision.at_ms
            );
            push_row(&mut out, &line);
        }
        out
    }

    /// Reads a log back out of `review.csv` (the writer's inverse).
    ///
    /// The decisions are rebuilt; the undo history is not (see the module
    /// docs). A later row for the same id wins, and the sequence counter
    /// continues after the highest number seen.
    ///
    /// # Errors
    ///
    /// [`TriageError::MissingColumn`] when a required column is absent,
    /// [`TriageError::Malformed`] for a row whose numbers or action do not
    /// parse, and the parser's own error for a broken quote.
    pub fn from_csv(text: &str) -> Result<Self, TriageError> {
        let rows = parse_csv(text, b',').map_err(|e| TriageError::Malformed {
            line: 1,
            reason: format!("{e}"),
        })?;
        let header = rows
            .first()
            .ok_or_else(|| TriageError::MissingColumn("seq".to_string()))?;
        let index_of = |name: &str| -> Result<usize, TriageError> {
            header
                .iter()
                .position(|h| h == name)
                .ok_or_else(|| TriageError::MissingColumn(name.to_string()))
        };
        let (seq_at, id_at, action_at, at_at) = (
            index_of("seq")?,
            index_of("id")?,
            index_of("action")?,
            index_of("at_ms")?,
        );
        let mut log = Self::new();
        for (row_index, row) in rows.iter().enumerate().skip(1) {
            let line = row_index + 1;
            let field = |at: usize| -> Result<&str, TriageError> {
                row.get(at)
                    .map(String::as_str)
                    .ok_or_else(|| TriageError::Malformed {
                        line,
                        reason: format!("only {} fields", row.len()),
                    })
            };
            let seq = field(seq_at)?
                .parse::<u64>()
                .map_err(|e| TriageError::Malformed {
                    line,
                    reason: format!("seq: {e}"),
                })?;
            let id = field(id_at)?
                .parse::<u32>()
                .map_err(|e| TriageError::Malformed {
                    line,
                    reason: format!("id: {e}"),
                })?;
            let action =
                TriageAction::parse(field(action_at)?).ok_or_else(|| TriageError::Malformed {
                    line,
                    reason: format!("action `{}`", field(action_at).unwrap_or("")),
                })?;
            let at_ms = field(at_at)?
                .parse::<u64>()
                .map_err(|e| TriageError::Malformed {
                    line,
                    reason: format!("at_ms: {e}"),
                })?;
            log.decisions
                .insert(id, TriageDecision { action, seq, at_ms });
            log.next_seq = log.next_seq.max(seq + 1);
        }
        Ok(log)
    }
}

/// Appends one already-comma-joined row, quoting only when a field demands it.
fn push_row(out: &mut String, line: &str) {
    let needs_quotes = line.contains(['"', '\n', '\r']);
    if !needs_quotes {
        out.push_str(line);
        out.push_str("\r\n");
        return;
    }
    out.push('"');
    for ch in line.chars() {
        if ch == '"' {
            out.push('"');
        }
        out.push(ch);
    }
    out.push('"');
    out.push_str("\r\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_decision_records_its_action_and_sequence() {
        let mut log = TriageLog::new();
        assert_eq!(log.apply(4, TriageAction::Approve, 1_700_000_000_000), 0);
        assert_eq!(log.apply(5, TriageAction::Reject, 1_700_000_000_100), 1);
        let decision = log.decision(4).expect("decided");
        assert_eq!(decision.action, TriageAction::Approve);
        assert_eq!(decision.seq, 0);
        assert_eq!(decision.at_ms, 1_700_000_000_000);
        assert_eq!(log.len(), 2);
        assert!(!log.is_empty());
        // Two decisions were taken, so both can be undone and then nothing can.
        assert!(log.can_undo());
        log.undo();
        log.undo();
        assert!(!log.can_undo(), "an empty history has nothing to undo");
    }

    #[test]
    fn undo_restores_the_previous_decision_not_an_empty_slot() {
        let mut log = TriageLog::new();
        log.apply(7, TriageAction::Flag, 10);
        log.apply(7, TriageAction::Reject, 20);
        let undone = log.undo().expect("something to undo");
        assert_eq!(undone.0, 7);
        assert_eq!(
            log.decision(7).map(|d| d.action),
            Some(TriageAction::Flag),
            "the flag comes back"
        );
        log.undo();
        assert_eq!(log.decision(7), None, "and then nothing");
        assert!(log.undo().is_none(), "an empty log has nothing to undo");
    }

    #[test]
    fn sequence_numbers_do_not_go_backwards_after_an_undo() {
        let mut log = TriageLog::new();
        log.apply(1, TriageAction::Approve, 0);
        log.undo();
        assert_eq!(log.apply(2, TriageAction::Approve, 0), 1);
    }

    #[test]
    fn counts_follow_the_action_order() {
        let mut log = TriageLog::new();
        log.apply(1, TriageAction::Approve, 0);
        log.apply(2, TriageAction::Approve, 0);
        log.apply(3, TriageAction::Duplicate, 0);
        log.apply(4, TriageAction::BulkApprove, 0);
        assert_eq!(log.counts(), [2, 0, 0, 1, 1]);
    }

    #[test]
    fn the_csv_is_chronological_whatever_the_id_order() {
        let mut log = TriageLog::new();
        log.apply(9, TriageAction::Reject, 111);
        log.apply(2, TriageAction::Approve, 222);
        let csv = log.to_csv();
        assert_eq!(
            csv,
            "seq,id,action,at_ms\r\n0,9,reject,111\r\n1,2,approve,222\r\n"
        );
    }

    #[test]
    fn the_csv_round_trips() {
        let mut log = TriageLog::new();
        log.apply(3, TriageAction::Flag, 10);
        log.apply(4, TriageAction::Approve, 20);
        let back = TriageLog::from_csv(&log.to_csv()).expect("parses");
        assert_eq!(back.len(), 2);
        assert_eq!(back.decision(3).map(|d| d.action), Some(TriageAction::Flag));
        assert_eq!(back.decision(4).map(|d| d.at_ms), Some(20));
        assert_eq!(back.to_csv(), log.to_csv(), "byte-identical re-export");
        // The sequence counter continues after the import.
        let mut back = back;
        assert_eq!(back.apply(5, TriageAction::Reject, 30), 2);
    }

    #[test]
    fn columns_may_be_in_any_order() {
        let text = "id,action,seq,at_ms\r\n7,duplicate,5,99\r\n";
        let log = TriageLog::from_csv(text).expect("parses");
        assert_eq!(
            log.decision(7).map(|d| d.action),
            Some(TriageAction::Duplicate)
        );
        assert_eq!(log.decision(7).map(|d| d.seq), Some(5));
    }

    #[test]
    fn malformed_rows_name_their_line() {
        let log = TriageLog::from_csv("seq,id,action,at_ms\r\n0,x,approve,1\r\n");
        assert_eq!(
            log,
            Err(TriageError::Malformed {
                line: 2,
                reason: "id: invalid digit found in string".to_string()
            })
        );
        let bad_action = TriageLog::from_csv("seq,id,action,at_ms\r\n0,1,shrug,1\r\n");
        assert!(matches!(
            bad_action,
            Err(TriageError::Malformed { line: 2, .. })
        ));
        let missing = TriageLog::from_csv("seq,id,at_ms\r\n0,1,1\r\n");
        assert_eq!(
            missing,
            Err(TriageError::MissingColumn("action".to_string()))
        );
    }

    #[test]
    fn a_later_row_for_the_same_icon_wins() {
        let text = "seq,id,action,at_ms\r\n0,1,flag,10\r\n1,1,approve,20\r\n";
        let log = TriageLog::from_csv(text).expect("parses");
        assert_eq!(log.len(), 1);
        assert_eq!(
            log.decision(1).map(|d| d.action),
            Some(TriageAction::Approve)
        );
        assert_eq!(log.counts(), [1, 0, 0, 0, 0]);
    }

    #[test]
    fn keys_and_names_round_trip() {
        for action in TriageAction::ALL {
            assert_eq!(TriageAction::parse(action.as_str()), Some(action));
            assert!(!action.key().is_empty());
        }
        assert_eq!(TriageAction::parse("nope"), None);
    }
}
