//! Atomic `.isgproj` save/open — the "kill mid-save still opens" guarantee.
//!
//! Model (ARCHITECTURE.md §4: a project file *is* a SQLite database):
//!
//! * The library is opened directly on the `.isgproj` file (WAL sidecars
//!   live next to it while the app runs).
//! * [`Library::save_in_place`] makes the file itself complete *first*
//!   (`wal_checkpoint(TRUNCATE)`), then writes a standalone copy via
//!   `VACUUM INTO` to a `*.tmp-<pid>` sibling, fsyncs it, and only then
//!   closes, atomically renames over the target, and reopens.
//!
//! A crash at *any* point leaves the target either the previous complete
//! snapshot or the new one; a partial `*.tmp-*` is inert (removed before the
//! next save) and never touched by open.

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::db::Library;
use crate::IsgError;

/// Fragment marking in-progress save temporaries (files named
/// `<target>.tmp-<pid>`). Such leftovers are inert and removed before every
/// save.
pub const TMP_MARKER: &str = ".tmp-";

fn tmp_path(target: &Path) -> PathBuf {
    let mut name = target
        .file_name()
        .map(|f| f.to_os_string())
        .unwrap_or_default();
    name.push(format!("{TMP_MARKER}{}", std::process::id()));
    let mut p = target.parent().unwrap_or(Path::new(".")).to_path_buf();
    p.push(name);
    p
}

fn remove_stale_tmp(target: &Path) {
    let dir = match target.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let Some(stem) = target.file_name() else {
        return;
    };
    let stem = stem.to_string_lossy();
    if let Ok(entries) = fs::read_dir(&dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with(&*stem) && name.contains(TMP_MARKER) {
                let _ = fs::remove_file(e.path());
            }
        }
    }
}

fn fsync_file(path: &Path) -> std::io::Result<()> {
    // Windows `FlushFileBuffers` requires a handle with GENERIC_WRITE; a
    // read-only handle (plain `File::open`) fails with Access Denied there
    // even though fsync(2) accepts read-only fds on Unix.
    let f = fs::OpenOptions::new().write(true).open(path)?;
    f.sync_all()
}

fn open_conn(path: &Path) -> crate::Result<Connection> {
    let conn = Connection::open(path)?;
    crate::db::apply_pragmas(&conn)?;
    Ok(conn)
}

impl Library {
    /// Saves the library to its own file atomically and leaves it open for
    /// further use (checkpoint → VACUUM INTO tmp → fsync → close → rename →
    /// reopen; see module docs).
    pub fn save_in_place(&mut self) -> crate::Result<()> {
        let Some(path) = self.path().map(Path::to_path_buf) else {
            return Err(IsgError::Corrupt(
                "in-memory library has no file path; use save_as".into(),
            ));
        };

        // 1) Make the main file itself complete (WAL → main db file).
        self.checkpoint_wal()?;

        // 2) Standalone copy into a temporary sibling.
        remove_stale_tmp(&path);
        let tmp = tmp_path(&path);
        // VACUUM INTO refuses to overwrite; any leftover was removed above.
        self.conn()
            .execute("VACUUM INTO ?1", rusqlite::params![&*tmp.to_string_lossy()])?;

        // 3) Durability before the swap.
        fsync_file(&tmp)?;

        // 4) Close, atomically replace, reopen. Between close and reopen the
        //    on-disk file is always a complete snapshot (old or new).
        let placeholder = Connection::open_in_memory()?;
        let conn = std::mem::replace(self.conn_mut(), placeholder);
        match conn.close() {
            Ok(()) => {}
            Err((conn, e)) => {
                // Could not close cleanly (e.g. statement still running):
                // put the connection back and surface the error.
                *self.conn_mut() = conn;
                return Err(e.into());
            }
        }
        let reopen = |slot: &mut Connection| -> crate::Result<()> {
            match open_conn(&path) {
                Ok(c) => {
                    *slot = c;
                    Ok(())
                }
                Err(e) => Err(e),
            }
        };
        if let Err(e) = fs::rename(&tmp, &path) {
            // The old snapshot is untouched; reopen it and surface the error.
            reopen(self.conn_mut())?;
            return Err(e.into());
        }
        reopen(self.conn_mut())?;
        Ok(())
    }

    /// Writes a snapshot of this library to `dest` (typically a different
    /// path); the live library stays untouched and open. Atomic by
    /// construction: `dest` is only ever touched by the final rename.
    pub fn save_as(&self, dest: &Path) -> crate::Result<()> {
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        remove_stale_tmp(dest);
        let tmp = tmp_path(dest);
        self.checkpoint_wal()?;
        self.conn()
            .execute("VACUUM INTO ?1", rusqlite::params![&*tmp.to_string_lossy()])?;
        fsync_file(&tmp)?;
        fs::rename(&tmp, dest)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{NewIcon, NewSheet, ReviewState};

    fn seed(lib: &mut Library, from: u32, n: u32) {
        for i in from..from + n {
            let mut id = [0u8; 16];
            id[..4].copy_from_slice(&i.to_le_bytes());
            lib.insert_sheet(&NewSheet {
                id,
                source_path: format!("sheet{i}.png"),
                content_hash: format!("hash-{i:08}"),
                width: 16,
                height: 16,
            })
            .unwrap();
        }
    }

    #[test]
    fn save_in_place_preserves_content_and_reopens() {
        let dir = std::env::temp_dir().join(format!("isg-proj-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("p.isgproj");
        let mut lib = Library::open(&path).unwrap();
        seed(&mut lib, 0, 10);
        lib.save_in_place().unwrap();
        assert_eq!(lib.sheet_count().unwrap(), 10, "still usable after save");
        assert_eq!(lib.path(), Some(path.as_path()));

        // A fresh open of the file sees all committed rows.
        drop(lib);
        let lib2 = Library::open(&path).unwrap();
        lib2.verify_integrity().unwrap();
        assert_eq!(lib2.sheet_count().unwrap(), 10);
        // Only complete snapshots remain (no .tmp leftovers after success).
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(TMP_MARKER))
            .collect();
        assert!(leftovers.is_empty(), "no tmp leftovers after clean save");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_as_snapshots_without_disturbing_source() {
        let dir = std::env::temp_dir().join(format!("isg-saveas-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let src = dir.join("a.isgproj");
        let dest = dir.join("sub").join("b.isgproj");
        let mut lib = Library::open(&src).unwrap();
        seed(&mut lib, 0, 3);
        lib.save_as(&dest).unwrap();

        {
            let snap = Library::open(&dest).unwrap();
            snap.verify_integrity().unwrap();
            assert_eq!(snap.sheet_count().unwrap(), 3);
        } // drop the snapshot handle: overwriting a file another connection
          // still holds open is denied on Windows.
          // Save-as snapshot excludes rows inserted afterwards.
        seed(&mut lib, 3, 2);
        lib.save_as(&dest).unwrap();
        let snap2 = Library::open(&dest).unwrap();
        assert_eq!(snap2.sheet_count().unwrap(), 5);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn review_state_roundtrip_and_audit_log() {
        let mut lib = Library::open_in_memory().unwrap();
        // Foreign keys are ON: the parent sheet must exist first.
        lib.insert_sheet(&NewSheet {
            id: [1u8; 16],
            source_path: "s.png".into(),
            content_hash: "h".into(),
            width: 16,
            height: 16,
        })
        .unwrap();
        let icon_id = [7u8; 16];
        lib.insert_icons(&[NewIcon {
            id: icon_id,
            sheet_id: [1u8; 16],
            bbox: (0, 0, 4, 4),
            preset: Some("mono-clean".into()),
            svg_path: None,
        }])
        .unwrap();
        lib.set_review_state(icon_id, ReviewState::Approved, "approve")
            .unwrap();
        let state: String = lib
            .test_conn()
            .query_row(
                "SELECT review_state FROM icons WHERE id = ?1",
                rusqlite::params![&icon_id[..]],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, "approved");
        let logs: i64 = lib
            .test_conn()
            .query_row("SELECT COUNT(*) FROM review_log", [], |r| r.get(0))
            .unwrap();
        assert_eq!(logs, 1);
    }
}
