//! The review surface, on the real library and the real detectors.
//!
//! The unit tests in `review_cmds.rs` cover the journal's own grammar and the
//! DTO mapping. What they cannot cover is the half that only exists once the
//! pieces are wired to the shipping library: a decision that lands in SQLite,
//! survives a close-and-reopen, moves the icon's `review_state` with it, and
//! comes back out of `review.csv` — and a pass whose icons keep the library's
//! ids and whose skipped rows leave gaps instead of renumbering everything after
//! them.
//!
//! These tests never touch Tauri: they call the same functions the commands in
//! `commands.rs` are thin wrappers around, so a failure here is a failure of the
//! surface itself rather than of a webview invocation.

use std::fs;
use std::path::PathBuf;

use isg_core::ForegroundMask;
use isg_native::db::{Library, NewIcon, NewSheet, ReviewState};
use isg_native::pipeline::SheetRaster;
use isg_native::review::TriageLog;
use isg_native::review_host::{host_review, HostIcon};
use isg_native::review_native::ReviewOptions;

use icon_forge_lib::review_cmds::{
    apply_decision, export, hex32, log_key, review_out, session_for, undo_decision, TriageActionDto,
};

/// The sheet every test in this file reviews.
const SHEET: [u8; 16] = [0x11; 16];

/// A second sheet, for the tests that need one session to stay out of another.
const OTHER: [u8; 16] = [0x22; 16];

/// The icon on row `n`, so a mix-up between a row and an id shows.
fn icon(n: u8) -> [u8; 16] {
    let mut id = [0xE0u8; 16];
    id[0] = n;
    id[15] = n.wrapping_mul(7);
    id
}

/// A project directory of this test's own, and the database file inside it.
///
/// Named by process id so two `cargo test` runs on one machine cannot collide,
/// and wiped first so a leftover file from a killed run cannot decide the result.
fn temp_project(name: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "icon-forge-review-surface-{}-{name}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("the temp project directory");
    let db = dir.join("library.db");
    (dir, db)
}

/// Writes one sheet and its icon rows, in the order the boxes are given.
///
/// `first` is the number of the sheet's first icon: icon ids are unique across
/// the whole library, so two sheets in one project cannot both start at one.
fn fill(lib: &mut Library, sheet: [u8; 16], hash: &str, first: u8, boxes: &[(u32, u32, u32, u32)]) {
    let outcome = lib
        .insert_sheet(&NewSheet {
            id: sheet,
            source_path: format!("{hash}.png"),
            content_hash: hash.to_string(),
            width: 128,
            height: 128,
        })
        .expect("the sheet row inserts");
    assert_eq!(
        outcome,
        isg_native::db::InsertOutcome::Inserted,
        "the sheet must be new"
    );
    let icons: Vec<NewIcon> = boxes
        .iter()
        .enumerate()
        .map(|(index, bbox)| NewIcon {
            id: icon(first + index as u8),
            sheet_id: sheet,
            bbox: *bbox,
            preset: Some("4".to_string()),
            svg_path: None,
        })
        .collect();
    assert_eq!(
        lib.insert_icons(&icons).expect("the icon rows insert"),
        icons.len()
    );
}

/// The sheet's icon ids, in the order the library returns its rows.
fn ids_of(lib: &Library, sheet: [u8; 16]) -> Vec<[u8; 16]> {
    lib.icons_for_sheet(&sheet)
        .expect("the sheet's rows")
        .iter()
        .map(|row| row.id)
        .collect()
}

/// The review states of a sheet's icons, in the same order as [`ids_of`].
fn states_of(lib: &Library, sheet: [u8; 16]) -> Vec<ReviewState> {
    lib.review_states_for(&sheet)
        .expect("the sheet's states")
        .into_iter()
        .map(|(_, state)| state)
        .collect()
}

#[test]
fn a_session_survives_a_reopen_with_its_states_and_its_journal() {
    let (dir, db) = temp_project("reopen");
    let boxes = [
        (0, 0, 32, 32),
        (32, 0, 32, 32),
        (64, 0, 32, 32),
        (96, 0, 32, 32),
    ];
    let mut lib = Library::open(&db).expect("the project opens");

    fill(&mut lib, SHEET, "surface-a", 1, &boxes);
    let ids = ids_of(&lib, SHEET);
    assert_eq!(ids.len(), 4, "one row per box");
    let mut session = session_for(SHEET, ids.clone(), &lib).expect("an empty journal");
    assert!(session.log.is_empty());

    // A decision moves the log, the icon's `review_state` and the journal.
    for (index, action) in [
        (0usize, TriageActionDto::Approve),
        (1, TriageActionDto::Reject),
        (2, TriageActionDto::Flag),
        (3, TriageActionDto::Duplicate),
    ] {
        let state = apply_decision(&mut session, &mut lib, ids[index], action).expect("a decision");
        assert_eq!(state.decided, index as u32 + 1);
        assert!(state.can_undo);
    }
    assert_eq!(
        states_of(&lib, SHEET),
        vec![
            ReviewState::Approved,
            ReviewState::Rejected,
            ReviewState::Flagged,
            ReviewState::Duplicate
        ],
        "the triage decision is the icon's review state"
    );

    // Undo takes the last one back in both records: the icon is pending again
    // and the journal still remembers both the decision and its reversal.
    let outcome = undo_decision(&mut session, &mut lib)
        .expect("an undo")
        .expect("four decisions were made");
    let (state, undone) = (outcome.triage.clone(), outcome.icon);
    assert_eq!(undone, ids[3]);
    assert_eq!(
        outcome.restored,
        ReviewState::Pending,
        "the duplicate had no earlier decision to go back to"
    );
    assert_eq!(state.decided, 3);
    assert_eq!(state.counts, [1, 1, 1, 0, 0]);
    assert_eq!(
        states_of(&lib, SHEET),
        vec![
            ReviewState::Approved,
            ReviewState::Rejected,
            ReviewState::Flagged,
            ReviewState::Pending
        ]
    );

    // The per-icon audit trail is keyed by the icon's own id, so it is not part
    // of the session's journal and cannot be replayed into one.
    let audit: Vec<String> = lib
        .review_log_for(&ids[3])
        .expect("the icon's audit rows")
        .into_iter()
        .map(|row| row.action)
        .collect();
    assert_eq!(audit, vec!["duplicate".to_string(), "undo".to_string()]);
    let rows = lib
        .review_log_for(&log_key(&SHEET))
        .expect("the session's journal");
    // Each decision is one event and the undo is another: an undo takes a
    // decision back, it does not erase the record of it.
    assert_eq!(
        rows.len(),
        5,
        "four decisions and the undo that took one back"
    );
    assert!(
        rows.iter().all(|row| row.action.starts_with("review/")),
        "the journal holds the session's events and nothing else: {rows:?}"
    );
    assert!(
        rows.iter().all(|row| row.icon_id == [0u8; 16]),
        "a 17-byte session key can never be read back as an icon id"
    );

    // The export is the record; it reads back into the same decisions.
    let export = export(&session);
    assert_eq!(export.decisions, 3);
    assert_eq!(export.seq, 4);
    let read_back = TriageLog::from_csv(&export.csv).expect("review.csv reads back");
    assert_eq!(read_back.counts(), session.log.counts());
    assert_eq!(read_back.len(), session.log.len());

    // Close and reopen: nothing was held in memory.
    let before = session.log.clone();
    let decisions_before = states_of(&lib, SHEET);
    drop(lib);
    let mut lib = Library::open(&db).expect("the project reopens");
    let reloaded = session_for(SHEET, ids_of(&lib, SHEET), &lib).expect("the journal");
    assert_eq!(reloaded.log, before, "the journal is the session's memory");
    assert_eq!(
        reloaded.state().seq,
        4,
        "sequence numbers carry across runs"
    );
    assert_eq!(
        states_of(&lib, SHEET),
        decisions_before,
        "and the icons still carry the states the decisions put them in"
    );

    // The reloaded session can still undo, and the undo reaches the file too.
    let mut reloaded = reloaded;
    let outcome = undo_decision(&mut reloaded, &mut lib)
        .expect("an undo")
        .expect("two decisions are left");
    let (state, undone) = (outcome.triage.clone(), outcome.icon);
    assert_eq!(undone, ids[2]);
    assert_eq!(
        outcome.restored,
        ReviewState::Pending,
        "icon 3 carries no earlier decision, so taking the flag back leaves it undecided"
    );
    assert_eq!(state.counts, [1, 1, 0, 0, 0]);
    drop(lib);
    let lib = Library::open(&db).expect("the project reopens again");
    assert_eq!(
        states_of(&lib, SHEET)[2],
        ReviewState::Pending,
        "the undo was written, not just computed"
    );
    drop(lib);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn two_sheets_keep_their_own_sessions() {
    let (dir, db) = temp_project("two-sheets");
    let mut lib = Library::open(&db).expect("the project opens");
    fill(
        &mut lib,
        SHEET,
        "surface-a",
        1,
        &[(0, 0, 32, 32), (32, 0, 32, 32)],
    );
    fill(&mut lib, OTHER, "surface-b", 11, &[(0, 64, 32, 32)]);

    let mut a = session_for(SHEET, ids_of(&lib, SHEET), &lib).expect("a's journal");
    let mut b = session_for(OTHER, ids_of(&lib, OTHER), &lib).expect("b's journal");
    apply_decision(&mut a, &mut lib, icon(1), TriageActionDto::Approve).expect("a decision in a");
    apply_decision(&mut b, &mut lib, icon(11), TriageActionDto::Reject).expect("a decision in b");

    let reloaded_a = session_for(SHEET, ids_of(&lib, SHEET), &lib).expect("a's journal");
    let reloaded_b = session_for(OTHER, ids_of(&lib, OTHER), &lib).expect("b's journal");
    assert_eq!(reloaded_a.log.len(), 1);
    assert_eq!(reloaded_b.log.len(), 1);
    assert_eq!(
        reloaded_a.log.counts(),
        [1, 0, 0, 0, 0],
        "a's decision is a's: each sheet's session is its own journal, even though both \
         sheets number their rows from zero"
    );
    assert_eq!(reloaded_b.log.counts(), [0, 1, 0, 0, 0]);
    assert_eq!(lib.review_log_for(&log_key(&SHEET)).unwrap().len(), 1);
    assert_eq!(lib.review_log_for(&log_key(&OTHER)).unwrap().len(), 1);
    assert_eq!(reloaded_a.log, a.log);

    // A decision that names an icon of another sheet is refused rather than
    // filed under a row it does not own.
    let error = apply_decision(&mut a, &mut lib, icon(11), TriageActionDto::Flag)
        .expect_err("icon 11 is a row of `OTHER`, not of this session's sheet");
    assert!(
        matches!(error, isg_native::IsgError::Corrupt(_)),
        "{error:?}"
    );
    assert_eq!(a.log.counts(), [1, 0, 0, 0, 0], "and nothing was recorded");
    drop(lib);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_pass_keeps_the_library_ids_and_the_gaps() {
    // A 128 × 32 sheet: two cells with ink, two without.
    let mut rgba = vec![255u8; 128 * 32 * 4];
    for x in 0..64 {
        for y in 0..32 {
            let at = (y * 128 + x) * 4;
            rgba[at] = 0;
            rgba[at + 1] = 0;
            rgba[at + 2] = 0;
        }
    }
    let sheet = SheetRaster::from_rgba(128, 32, rgba);
    let mut mask = ForegroundMask::new(128, 32);
    for x in 0..64 {
        for y in 0..32 {
            mask.set(x, y, true);
        }
    }

    // Two icons share one tracing byte for byte (a copy); the other two are
    // drawn but cannot be reviewed — one box has no ink under it, one is empty.
    let document = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"32\" height=\"32\">\
                    <rect width=\"32\" height=\"32\" fill=\"#000000\"/></svg>";
    let other = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"32\" height=\"32\">\
                 <rect x=\"8\" y=\"8\" width=\"16\" height=\"16\" fill=\"#000000\"/></svg>";
    let icons = vec![
        HostIcon {
            id: icon(1),
            bbox: (0, 0, 32, 32),
            document: Some(document.to_string()),
        },
        HostIcon {
            id: icon(2),
            bbox: (32, 0, 32, 32),
            document: Some(document.to_string()),
        },
        HostIcon {
            id: icon(3),
            bbox: (64, 0, 32, 32),
            document: Some(other.to_string()),
        },
        HostIcon {
            id: icon(4),
            bbox: (0, 0, 0, 0),
            document: Some(other.to_string()),
        },
    ];
    let report = host_review(&sheet, &mask, &icons, &ReviewOptions::default())
        .expect("a pass over four rows");

    // The reviewed icons carry the library's ids, and their index is the row
    // they came from — not a fresh numbering of the ones that survived.
    let reviewed: Vec<(u32, [u8; 16])> = report
        .icons
        .iter()
        .map(|entry| (entry.index, entry.id))
        .collect();
    assert_eq!(reviewed, vec![(0, icon(1)), (1, icon(2))]);
    // The refused rows are reported with their own ids and keep the gap.
    let skipped: Vec<([u8; 16], String)> = report
        .skipped
        .iter()
        .map(|skip| (skip.id, skip.reason.clone()))
        .collect();
    assert_eq!(skipped.len(), 2);
    assert_eq!(skipped[0].0, icon(3));
    assert!(skipped[0].1.contains("no ink"), "{}", skipped[0].1);
    assert_eq!(skipped[1].0, icon(4));
    assert!(skipped[1].1.contains("empty"), "{}", skipped[1].1);
    // Byte-identical tracings are one cluster, named in library ids.
    assert_eq!(report.clusters.len(), 1, "{:?}", report.clusters);
    assert_eq!(report.clusters[0].members, vec![icon(1), icon(2)]);
    assert_eq!(
        report.duplicate_ids(),
        [icon(1), icon(2)].into_iter().collect()
    );

    // And the webview's view of the same pass names those ids, nothing else.
    let session = icon_forge_lib::review_cmds::ReviewSession::new(
        SHEET,
        vec![icon(1), icon(2), icon(3), icon(4)],
    );
    let out = review_out(&report, &session);
    assert_eq!(out.sheet, hex32(SHEET));
    assert_eq!(out.icons.len(), 2);
    assert_eq!(out.icons[0].id, hex32(icon(1)));
    assert_eq!(out.icons[1].index, 1);
    assert_eq!(out.icons[0].state, "pending");
    assert_eq!(out.skipped.len(), 2);
    assert_eq!(out.skipped[0].id, hex32(icon(3)));
    assert_eq!(
        out.clusters[0].members,
        vec![hex32(icon(1)), hex32(icon(2))]
    );
    assert!(
        out.clusters[0].identical,
        "the two tracings are byte-identical"
    );
    assert_eq!(
        out.icons[0].cluster.as_ref().map(|c| c.identical),
        Some(true)
    );
    assert_eq!(out.triage.decided, 0);
    assert!(!out.triage.can_undo);
}

#[test]
fn the_measured_crop_is_the_box_on_the_sheet() {
    // One icon whose document draws a shape the crop does not contain: the pass
    // still reviews it (the score is what says so) — the reviewed set is decided
    // by ink and boxes, never by a score threshold, so a bad trace is still a
    // row a reviewer can decide on.
    let mut rgba = vec![255u8; 32 * 32 * 4];
    for x in 0..16 {
        for y in 0..16 {
            let at = (y * 32 + x) * 4;
            rgba[at] = 0;
            rgba[at + 1] = 0;
            rgba[at + 2] = 0;
        }
    }
    let sheet = SheetRaster::from_rgba(32, 32, rgba);
    let mut mask = ForegroundMask::new(32, 32);
    for x in 0..16 {
        for y in 0..16 {
            mask.set(x, y, true);
        }
    }
    // A traced document is the engine's own dialect — `<path>` data, never a
    // `<rect>`: the review counts the segments the engine's parser produced, and
    // a `<rect>` produces none. Four lines back to the start is the traced
    // square; `Z` closes it without adding a fifth segment.
    let document = concat!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16">"##,
        r##"<path d="M0,0L4,0L4,4L0,4L0,0Z" fill="#000000"/></svg>"##
    );
    let icons = vec![HostIcon {
        id: icon(1),
        bbox: (0, 0, 16, 16),
        document: Some(document.to_string()),
    }];
    let report = host_review(&sheet, &mask, &icons, &ReviewOptions::default())
        .expect("a pass over one icon");
    assert_eq!(report.icons.len(), 1, "{:?}", report.skipped);
    assert!(report.skipped.is_empty());
    let review = &report.icons[0].review;
    assert_eq!(review.id, 0, "the detectors' key is the row");
    assert_eq!(review.ink_area, 256, "16 × 16 of ink in the box");
    assert!(
        review.stat.ink_size > 0.0,
        "the box's metrics were measured: {:?}",
        review.stat
    );
    assert!(
        review.digest != [0u8; 32],
        "the cell was hashed, not left at zero"
    );
    assert_eq!(
        review.node_count, 4,
        "the traced square is four lines — `Z` closes, it does not add a segment"
    );
    assert!(review.closed, "the document's `Z` closed the outline");
    assert_eq!(review.colours, 1, "one fill in the document");
}
