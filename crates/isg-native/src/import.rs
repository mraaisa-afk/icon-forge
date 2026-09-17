//! Streaming folder import (ARCHITECTURE.md Phase 1 "streaming import").
//!
//! Deterministic order (directory-sorted walk), header-only dimension reads
//! (no full decode at import time), blake3 content hashing for
//! deduplication, and transactions batched at 256 files so cancellation
//! latency stays well under a second and memory stays O(batch), not O(n).

use std::fs;
use std::path::Path;

use blake3::Hasher;
use walkdir::WalkDir;

use crate::cancel::CancellationToken;
use crate::db::{InsertOutcome, Library, NewSheet};
use crate::IsgError;

/// Import tuning knobs.
#[derive(Debug, Clone, Copy)]
pub struct ImportOptions {
    /// Files larger than this are skipped (counted in `scanned`, not hashed).
    pub max_file_bytes: u64,
    /// Transaction batch size (files per commit).
    pub batch_size: usize,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            max_file_bytes: 256 * 1024 * 1024,
            batch_size: 256,
        }
    }
}

/// Progress snapshot passed to the import callback.
#[derive(Debug, Clone, Copy)]
pub struct ImportProgress {
    /// Files processed so far (imported + skipped, excluding unread).
    pub processed: u32,
    /// Files discovered so far.
    pub scanned: u32,
    /// Rows newly inserted so far.
    pub imported: u32,
}

/// Final import summary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImportStats {
    /// Candidate files discovered.
    pub scanned: u32,
    /// Rows newly inserted.
    pub imported: u32,
    /// Files skipped because identical content was already in the library.
    pub skipped_duplicate: u32,
    /// Files that could not be read or dimension-probed.
    pub corrupt: u32,
    /// Sum of hashed file sizes.
    pub total_bytes: u64,
    /// True when the run stopped early because the token fired.
    pub cancelled: bool,
}

/// Recursively imports raster files (`png`/`jpg`/`jpeg`, case-insensitive)
/// from `root` into `lib`. `progress` is invoked once per batch and once at
/// the end.
pub fn import_folder(
    lib: &mut Library,
    root: &Path,
    opts: &ImportOptions,
    cancel: &CancellationToken,
    progress: &mut dyn FnMut(ImportProgress),
) -> crate::Result<ImportStats> {
    if !root.is_dir() {
        return Err(IsgError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("not a directory: {}", root.display()),
        )));
    }

    let mut stats = ImportStats::default();
    let mut batch: Vec<NewSheet> = Vec::with_capacity(opts.batch_size);

    let walker = WalkDir::new(root).sort_by_file_name().into_iter();
    for entry in walker {
        if cancel.is_cancelled() {
            stats.cancelled = true;
            break;
        }
        let entry = match entry {
            Ok(e) => e,
            Err(_) => {
                stats.corrupt += 1; // unreadable directory entry
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let ext_ok = entry
            .path()
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| {
                let e = e.to_ascii_lowercase();
                e == "png" || e == "jpg" || e == "jpeg"
            });
        if !ext_ok {
            continue;
        }
        stats.scanned += 1;

        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => {
                stats.corrupt += 1;
                continue;
            }
        };
        if meta.len() > opts.max_file_bytes {
            stats.corrupt += 1;
            continue;
        }

        let bytes = match fs::read(entry.path()) {
            Ok(b) => b,
            Err(_) => {
                stats.corrupt += 1;
                continue;
            }
        };
        let (width, height) = match image::image_dimensions(entry.path()) {
            Ok(d) => d,
            Err(_) => {
                stats.corrupt += 1;
                continue;
            }
        };

        let mut hasher = Hasher::new();
        hasher.update(&bytes);
        let digest = hasher.finalize();
        stats.total_bytes += bytes.len() as u64;

        let mut id = [0u8; 16];
        id.copy_from_slice(&digest.as_bytes()[..16]);

        batch.push(NewSheet {
            id,
            source_path: entry.path().to_string_lossy().into_owned(),
            content_hash: digest.to_hex().to_string(),
            width,
            height,
        });

        if batch.len() >= opts.batch_size {
            flush(lib, &mut batch, &mut stats)?;
            progress(ImportProgress {
                processed: stats.imported + stats.skipped_duplicate + stats.corrupt,
                scanned: stats.scanned,
                imported: stats.imported,
            });
        }
    }

    if !batch.is_empty() && !cancel.is_cancelled() {
        flush(lib, &mut batch, &mut stats)?;
    }
    progress(ImportProgress {
        processed: stats.imported + stats.skipped_duplicate + stats.corrupt,
        scanned: stats.scanned,
        imported: stats.imported,
    });
    Ok(stats)
}

fn flush(
    lib: &mut Library,
    batch: &mut Vec<NewSheet>,
    stats: &mut ImportStats,
) -> crate::Result<()> {
    let tx = lib.conn_mut().transaction()?;
    for sheet in batch.iter() {
        let outcome = insert_in_tx(&tx, sheet)?;
        match outcome {
            InsertOutcome::Inserted => stats.imported += 1,
            InsertOutcome::Duplicate => stats.skipped_duplicate += 1,
        }
    }
    tx.commit()?;
    batch.clear();
    Ok(())
}

fn insert_in_tx(tx: &rusqlite::Transaction<'_>, sheet: &NewSheet) -> crate::Result<InsertOutcome> {
    let changed = tx.execute(
        "INSERT OR IGNORE INTO sheets (id, source_path, content_hash, width, height, imported_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            &sheet.id[..],
            sheet.source_path,
            sheet.content_hash,
            sheet.width,
            sheet.height,
            crate::db::now_millis(),
        ],
    )?;
    Ok(if changed == 0 {
        InsertOutcome::Duplicate
    } else {
        InsertOutcome::Inserted
    })
}
