//! The SQLite library — schema per ARCHITECTURE.md §4 (`sheets`, `icons`,
//! `cache`, `review_log`), WAL journal mode, incremental migrations keyed by
//! `PRAGMA user_version`.
//!
//! One process-wide [`Library`] backs the app; `.isgproj` files are
//! snapshots of it (see [`crate::project`]).

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;

use crate::IsgError;

/// Schema version of a freshly created database.
pub const SCHEMA_VERSION: i64 = 1;

/// Opened library database (WAL mode).
pub struct Library {
    conn: Connection,
    path_buf: Option<PathBuf>,
}

/// Outcome of an insert that is unique-keyed on `content_hash`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertOutcome {
    /// The row was newly inserted.
    Inserted,
    /// An identical row already existed (hash match); nothing changed.
    Duplicate,
}

/// A sheet row to insert (new sheet import).
#[derive(Debug, Clone)]
pub struct NewSheet {
    /// 16-byte id (deterministic: first 16 bytes of the blake3 content hash —
    /// re-importing identical content into another project yields the same id).
    pub id: [u8; 16],
    /// Source file path as imported.
    pub source_path: String,
    /// blake3 hex digest of the source bytes.
    pub content_hash: String,
    /// Image width in pixels (from the format header — no full decode).
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
}

/// An icon row to insert (grouping output; Phase 3 fills the pipeline in).
#[derive(Debug, Clone)]
pub struct NewIcon {
    /// 16-byte id (same derivation rule as sheets).
    pub id: [u8; 16],
    /// Owning sheet id.
    pub sheet_id: [u8; 16],
    /// Tight bbox of the icon inside the sheet.
    pub bbox: (u32, u32, u32, u32),
    /// Optional preset name used for tracing.
    pub preset: Option<String>,
    /// Optional cached-SVG relative path.
    pub svg_path: Option<String>,
}

/// One sheet's vectorized icon row (grouping bbox + stage ⑤–⑧ outcome).
#[derive(Debug, Clone)]
pub struct IconVectorRow {
    /// 16-byte id (deterministic: blake3 of sheet content hash ‖ bbox).
    pub id: [u8; 16],
    /// Tight bbox `(x, y, w, h)` inside the sheet.
    pub bbox: (u32, u32, u32, u32),
    /// Stage ⑧ cache key (the SVG lives in the cache payload).
    pub svg_key: String,
    /// Preset doc name used for tracing.
    pub preset: String,
    /// Stage ⑧ MAE.
    pub mae: f32,
    /// Stage ⑧ SSIM.
    pub ssim: f32,
    /// Stage ⑧ IoU.
    pub iou: f32,
}

/// A paged sheet row for the library grid.
#[derive(Debug, Clone)]
pub struct SheetRow {
    /// 16-byte id.
    pub id: Vec<u8>,
    /// Source path.
    pub source_path: String,
    /// blake3 hex digest.
    pub content_hash: String,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Import time (unix epoch milliseconds, stored as TEXT).
    pub imported_at: String,
}

/// Review states of an icon (ARCHITECTURE.md §4 `review_state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewState {
    /// Not yet triaged.
    Pending,
    /// Approved by the user.
    Approved,
    /// Rejected by the user.
    Rejected,
    /// Flagged for follow-up.
    Flagged,
    /// Marked as duplicate of another icon.
    Duplicate,
}

impl ReviewState {
    /// Canonical lower-case name stored in the DB.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ReviewState::Pending => "pending",
            ReviewState::Approved => "approved",
            ReviewState::Rejected => "rejected",
            ReviewState::Flagged => "flagged",
            ReviewState::Duplicate => "duplicate",
        }
    }
}

impl fmt::Display for Library {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Library({} sheets)", self.sheet_count().unwrap_or(0))
    }
}

/// Unix epoch milliseconds as string (the `imported_at` / `timestamp` column
/// format; lexicographic order equals chronological order until year 2286).
#[must_use]
pub fn now_millis() -> String {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    ms.to_string()
}

pub(crate) fn apply_pragmas(conn: &Connection) -> crate::Result<()> {
    // journal_mode returns the effective mode as a row — read it back so a
    // silent fallback to `delete` (e.g. on exotic filesystems) is visible.
    let mode: String = conn.query_row("PRAGMA journal_mode=WAL;", [], |r| r.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(IsgError::Corrupt(format!(
            "journal_mode is '{mode}', expected wal"
        )));
    }
    finish_pragmas(conn)
}

/// Pragmas for in-memory libraries (tests only): WAL requires a file-backed
/// database, so the memory journal is the correct — and asserted — mode.
pub(crate) fn apply_memory_pragmas(conn: &Connection) -> crate::Result<()> {
    conn.execute_batch("PRAGMA journal_mode=MEMORY;")?;
    finish_pragmas(conn)
}

fn finish_pragmas(conn: &Connection) -> crate::Result<()> {
    conn.execute_batch(
        "PRAGMA synchronous=NORMAL;
         PRAGMA foreign_keys=ON;
         PRAGMA busy_timeout=5000;",
    )?;
    Ok(())
}

fn migrate(conn: &Connection) -> crate::Result<()> {
    let v: i64 = conn.query_row("PRAGMA user_version;", [], |r| r.get(0))?;
    if v >= SCHEMA_VERSION {
        return Ok(());
    }
    if v < 1 {
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE IF NOT EXISTS sheets (
                 id            BLOB PRIMARY KEY,
                 source_path   TEXT NOT NULL,
                 content_hash  TEXT NOT NULL UNIQUE,
                 width         INTEGER NOT NULL,
                 height        INTEGER NOT NULL,
                 imported_at   TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS icons (
                 id            BLOB PRIMARY KEY,
                 sheet_id      BLOB NOT NULL REFERENCES sheets(id),
                 bbox_x        INTEGER NOT NULL,
                 bbox_y        INTEGER NOT NULL,
                 bbox_w        INTEGER NOT NULL,
                 bbox_h        INTEGER NOT NULL,
                 svg_path      TEXT,
                 preset        TEXT,
                 quality_mae   REAL,
                 quality_ssim  REAL,
                 quality_iou   REAL,
                 confidence    REAL,
                 group_id      BLOB,
                 review_state  TEXT NOT NULL DEFAULT 'pending',
                 reviewed_at   TEXT,
                 stroke_weight REAL,
                 solidity      REAL,
                 ink_area      INTEGER
             );
             CREATE INDEX IF NOT EXISTS icons_sheet ON icons(sheet_id);
             CREATE INDEX IF NOT EXISTS icons_review_state ON icons(review_state);
             CREATE TABLE IF NOT EXISTS cache (
                 key           TEXT PRIMARY KEY,
                 payload_path  TEXT NOT NULL,
                 created_at    TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS review_log (
                 id            INTEGER PRIMARY KEY AUTOINCREMENT,
                 icon_id       BLOB NOT NULL,
                 action        TEXT NOT NULL,
                 timestamp     TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS review_log_icon ON review_log(icon_id);
             COMMIT;
             PRAGMA user_version = 1;",
        )?;
    }
    Ok(())
}

impl Library {
    /// Opens (creating if needed) the library database at `path` and runs
    /// migrations. Leaves stale `*.tmp-*` save leftovers alone — they are
    /// inert by construction (see [`crate::project`]).
    pub fn open(path: &Path) -> crate::Result<Self> {
        let conn = Connection::open(path)?;
        apply_pragmas(&conn)?;
        migrate(&conn)?;
        Ok(Self {
            conn,
            path_buf: Some(path.to_path_buf()),
        })
    }

    /// In-memory library for tests (memory journal — see
    /// [`apply_memory_pragmas`]).
    pub fn open_in_memory() -> crate::Result<Self> {
        let conn = Connection::open_in_memory()?;
        apply_memory_pragmas(&conn)?;
        migrate(&conn)?;
        Ok(Self {
            conn,
            path_buf: None,
        })
    }

    /// Returns the path the library was opened from (`None` for in-memory).
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path_buf.as_deref()
    }

    /// Re-applies connection pragmas (used after a save reopen).
    /// Runs `PRAGMA quick_check` and validates the schema version.
    pub fn verify_integrity(&self) -> crate::Result<()> {
        let status: String = self
            .conn
            .query_row("PRAGMA quick_check;", [], |r| r.get(0))?;
        if status != "ok" {
            return Err(IsgError::Corrupt(format!("quick_check: {status}")));
        }
        let v: i64 = self
            .conn
            .query_row("PRAGMA user_version;", [], |r| r.get(0))?;
        if v != SCHEMA_VERSION {
            return Err(IsgError::Corrupt(format!(
                "schema version {v}, expected {SCHEMA_VERSION}"
            )));
        }
        Ok(())
    }

    /// Read-only handle for modules that compose SQL here (kept `pub(crate)`
    /// so the schema stays an implementation detail of this crate).
    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Mutable handle for transactional batches.
    pub(crate) fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    /// Inserts a sheet row (deduplicated on `content_hash`).
    pub fn insert_sheet(&mut self, sheet: &NewSheet) -> crate::Result<InsertOutcome> {
        let changed = self.conn.execute(
            "INSERT OR IGNORE INTO sheets (id, source_path, content_hash, width, height, imported_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                &sheet.id[..],
                sheet.source_path,
                sheet.content_hash,
                sheet.width,
                sheet.height,
                now_millis(),
            ],
        )?;
        Ok(if changed == 0 {
            InsertOutcome::Duplicate
        } else {
            InsertOutcome::Inserted
        })
    }

    /// Inserts icon rows in one transaction.
    pub fn insert_icons(&mut self, icons: &[NewIcon]) -> crate::Result<usize> {
        let tx = self.conn.transaction()?;
        let mut inserted = 0usize;
        for icon in icons {
            inserted += tx.execute(
                "INSERT OR IGNORE INTO icons
                     (id, sheet_id, bbox_x, bbox_y, bbox_w, bbox_h, preset, svg_path, review_state)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'pending')",
                rusqlite::params![
                    &icon.id[..],
                    &icon.sheet_id[..],
                    icon.bbox.0,
                    icon.bbox.1,
                    icon.bbox.2,
                    icon.bbox.3,
                    icon.preset,
                    icon.svg_path,
                ],
            )?;
        }
        tx.commit()?;
        Ok(inserted)
    }

    /// Paged sheet listing for the virtualized library grid (newest first).
    pub fn list_sheets(&self, offset: u64, limit: u32) -> crate::Result<Vec<SheetRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source_path, content_hash, width, height, imported_at
             FROM sheets ORDER BY imported_at DESC, id LIMIT ?1 OFFSET ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![limit, offset as i64], |r| {
            Ok(SheetRow {
                id: r.get(0)?,
                source_path: r.get(1)?,
                content_hash: r.get(2)?,
                width: r.get(3)?,
                height: r.get(4)?,
                imported_at: r.get(5)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Total sheet count (library header / pagination).
    /// Fetches one sheet row by its 16-byte id.
    pub fn sheet_by_id(&self, id: &[u8]) -> crate::Result<Option<SheetRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source_path, content_hash, width, height, imported_at
             FROM sheets WHERE id = ?1",
        )?;
        let mut rows = stmt.query(rusqlite::params![id])?;
        match rows.next()? {
            None => Ok(None),
            Some(r) => Ok(Some(SheetRow {
                id: r.get(0)?,
                source_path: r.get(1)?,
                content_hash: r.get(2)?,
                width: r.get(3)?,
                height: r.get(4)?,
                imported_at: r.get(5)?,
            })),
        }
    }

    /// Light rows for one sheet's icons (library grid + review lists).
    pub fn icons_for_sheet(&self, sheet_id: &[u8]) -> crate::Result<Vec<IconVectorRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, bbox_x, bbox_y, bbox_w, bbox_h, svg_path, preset,
                    quality_mae, quality_ssim, quality_iou
             FROM icons WHERE sheet_id = ?1 ORDER BY bbox_y, bbox_x",
        )?;
        let mut out = Vec::new();
        let mut rows = stmt.query(rusqlite::params![sheet_id])?;
        while let Some(r) = rows.next()? {
            let id: Vec<u8> = r.get(0)?;
            let mut id_arr = [0u8; 16];
            id_arr.copy_from_slice(&id[..16]);
            out.push(IconVectorRow {
                id: id_arr,
                bbox: (r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?),
                svg_key: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                preset: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
                mae: r.get::<_, Option<f32>>(7)?.unwrap_or(0.0),
                ssim: r.get::<_, Option<f32>>(8)?.unwrap_or(0.0),
                iou: r.get::<_, Option<f32>>(9)?.unwrap_or(0.0),
            });
        }
        Ok(out)
    }

    /// Replaces one sheet's icon rows with `rows` (deterministic grouping
    /// makes the set stable, so delete+insert is idempotent). One
    /// transaction; `review_state` resets to `pending` for fresh rows.
    pub fn replace_sheet_icons(
        &mut self,
        sheet_id: &[u8],
        rows: &[IconVectorRow],
    ) -> crate::Result<usize> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM icons WHERE sheet_id = ?1", rusqlite::params![sheet_id])?;
        let mut inserted = 0usize;
        for r in rows {
            tx.execute(
                "INSERT INTO icons (id, sheet_id, bbox_x, bbox_y, bbox_w, bbox_h,
                                    svg_path, preset, quality_mae, quality_ssim, quality_iou)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                rusqlite::params![
                    &r.id[..],
                    sheet_id,
                    r.bbox.0,
                    r.bbox.1,
                    r.bbox.2,
                    r.bbox.3,
                    if r.svg_key.is_empty() { None } else { Some(&r.svg_key) },
                    if r.preset.is_empty() { None } else { Some(&r.preset) },
                    r.mae,
                    r.ssim,
                    r.iou,
                ],
            )?;
            inserted += 1;
        }
        tx.commit()?;
        Ok(inserted)
    }

    /// Number of sheets stored in the library.
    pub fn sheet_count(&self) -> crate::Result<u64> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM sheets", [], |r| r.get(0))?;
        Ok(n.max(0) as u64)
    }

    /// Total icon count.
    pub fn icon_count(&self) -> crate::Result<u64> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM icons", [], |r| r.get(0))?;
        Ok(n.max(0) as u64)
    }

    /// Sets an icon's review state and appends to the audit log.
    pub fn set_review_state(
        &mut self,
        icon_id: [u8; 16],
        state: ReviewState,
        action: &str,
    ) -> crate::Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE icons SET review_state = ?1, reviewed_at = ?2 WHERE id = ?3",
            rusqlite::params![state.as_str(), now_millis(), &icon_id[..]],
        )?;
        tx.execute(
            "INSERT INTO review_log (icon_id, action, timestamp) VALUES (?1, ?2, ?3)",
            rusqlite::params![&icon_id[..], action, now_millis()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Inserts or replaces a cache index row (`key → payload_path`).
    pub fn cache_put(&self, key: &str, payload_path: &str) -> crate::Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO cache (key, payload_path, created_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![key, payload_path, now_millis()],
        )?;
        Ok(())
    }

    /// Looks up a cache payload path.
    #[must_use]
    pub fn cache_get(&self, key: &str) -> Option<String> {
        self.conn
            .query_row(
                "SELECT payload_path FROM cache WHERE key = ?1",
                rusqlite::params![key],
                |r| r.get(0),
            )
            .ok()
    }

    /// Checkpoints the WAL into the main file (`TRUNCATE`), so the `.db`
    /// file alone is a complete snapshot — the precondition for atomic
    /// saves and safe file copies.
    pub fn checkpoint_wal(&self) -> crate::Result<()> {
        // wal_checkpoint returns a result row; sqlite3_exec (execute_batch)
        // discards rows, which is exactly what we want.
        self.conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(())
    }

    /// Exposes the raw connection path for tests only.
    #[cfg(test)]
    pub(crate) fn test_conn(&self) -> &Connection {
        &self.conn
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet_row() -> NewSheet {
        NewSheet {
            id: [9; 16],
            source_path: "C:\\sheets\\a.png".to_string(),
            content_hash: "cafe1234".to_string(),
            width: 96,
            height: 48,
        }
    }

    fn vector_row(bbox: (u32, u32, u32, u32)) -> IconVectorRow {
        IconVectorRow {
            id: [1; 16],
            bbox,
            svg_key: format!("key-{}-{}", bbox.0, bbox.1),
            preset: "mono-fast".to_string(),
            mae: 0.01,
            ssim: 0.99,
            iou: 0.98,
        }
    }

    #[test]
    fn sheet_lookup_and_icon_replace_roundtrip() {
        let mut lib = Library::open_in_memory().unwrap();
        assert_eq!(
            lib.insert_sheet(&sheet_row()).unwrap(),
            InsertOutcome::Inserted
        );
        let found = lib.sheet_by_id(&[9; 16]).unwrap().expect("row exists");
        assert_eq!(found.content_hash, "cafe1234");
        assert_eq!(found.width, 96);
        assert!(lib.sheet_by_id(&[8; 16]).unwrap().is_none());

        let rows = vec![
            vector_row((8, 8, 16, 16)),
            vector_row((56, 24, 16, 16)),
        ];
        assert_eq!(lib.replace_sheet_icons(&[9; 16], &rows).unwrap(), 2);
        assert_eq!(lib.icon_count().unwrap(), 2);

        let back = lib.icons_for_sheet(&[9; 16]).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].bbox, (8, 8, 16, 16));
        assert_eq!(back[0].svg_key, "key-8-8");
        assert_eq!(back[0].preset, "mono-fast");
        assert!(back[0].ssim > 0.98);

        // Replacing the same sheet's icons is idempotent.
        assert_eq!(lib.replace_sheet_icons(&[9; 16], &rows).unwrap(), 2);
        assert_eq!(lib.icon_count().unwrap(), 2);
        assert!(lib.icons_for_sheet(&[1; 16]).unwrap().is_empty());
    }
}
