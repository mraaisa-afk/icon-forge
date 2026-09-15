//! Job-engine wiring: spawns the T0/T1/T2 [`JobEngine`] with a sink that
//! forwards [`JobEvent`]s to the webview as `job://event` payloads, and
//! provides the import job implementation.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use isg_core::TracePreset;
use isg_native::cache::CacheStore;
use isg_native::db::Library;
use isg_native::import::{import_folder, ImportOptions};
use isg_native::jobs::{Job, JobContext, JobEngine, JobError, JobEvent, JobOutcome, Tier};
use isg_native::pipeline::{vectorize_sheet_batch, BatchError, BatchOptions, SheetRef};

/// Event name used for all job events (payload = [`JobEventDto`]).
pub const JOB_EVENT: &str = "job://event";

/// Serializable mirror of [`JobEvent`] for the webview.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum JobEventDto {
    /// A job started executing.
    Started {
        /// Engine-assigned job id.
        id: u64,
        /// Job name.
        name: String,
    },
    /// Job-reported progress.
    Progress {
        /// Engine-assigned job id.
        id: u64,
        /// Completed units.
        done: u64,
        /// Total units (0 = unknown yet).
        total: u64,
        /// Message.
        message: String,
    },
    /// Terminal state.
    Finished {
        /// Engine-assigned job id.
        id: u64,
        /// Outcome tag (`succeeded` | `cancelled` | `preempted` | `failed`).
        outcome: &'static str,
        /// Detail message (success summary or failure reason).
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
}

impl From<JobEvent> for JobEventDto {
    fn from(e: JobEvent) -> Self {
        match e {
            JobEvent::Started { id, name } => Self::Started { id: id.0, name },
            JobEvent::Progress {
                id,
                done,
                total,
                message,
            } => Self::Progress {
                id: id.0,
                done,
                total,
                message,
            },
            JobEvent::Finished { id, outcome } => {
                let (tag, message) = match outcome {
                    JobOutcome::Succeeded(m) => ("succeeded", Some(m)),
                    JobOutcome::Cancelled => ("cancelled", None),
                    JobOutcome::Preempted => ("preempted", None),
                    JobOutcome::Failed(m) => ("failed", Some(m)),
                };
                Self::Finished {
                    id: id.0,
                    outcome: tag,
                    message,
                }
            }
        }
    }
}

/// Spawns the engine whose events are emitted to all webview windows.
#[must_use]
pub fn spawn_engine(handle: AppHandle) -> JobEngine {
    JobEngine::new(Arc::new(move |event: JobEvent| {
        let dto = JobEventDto::from(event);
        // A UI that has navigated away must not kill the worker thread.
        let _ = handle.emit(JOB_EVENT, &dto);
    }))
}

/// T2 batch job: streaming folder import into the app library.
///
/// The library slot is held for the duration of the import (one sequential
/// pass); WAL snapshot reads keep the UI's read commands unblocked while it
/// runs.
pub struct ImportJob {
    /// Folder to import.
    pub root: PathBuf,
    /// Shared with [`crate::state::App::library`].
    pub library_slot: Arc<Mutex<Option<Library>>>,
}

/// Cache directory for a project: `vector-cache` next to the project file
/// (temp dir fallback for in-memory libraries — tests only).
#[must_use]
pub fn cache_dir_for(db_path: Option<&std::path::Path>) -> PathBuf {
    match db_path.and_then(std::path::Path::parent) {
        Some(dir) => dir.join("vector-cache"),
        None => std::env::temp_dir().join("icon-forge-vector-cache"),
    }
}

/// T2 bulk vectorization of one stored sheet through stages ①–⑧.
pub struct VectorizeSheetJob {
    /// Sheet to vectorize (id as stored in the library).
    pub sheet_id: [u8; 16],
    /// Trace preset for every icon on the sheet.
    pub preset: TracePreset,
    /// Shared with [`crate::state::App::library`].
    pub library_slot: Arc<Mutex<Option<Library>>>,
}

impl Job for VectorizeSheetJob {
    fn name(&self) -> &str {
        "Vectorize sheet"
    }

    fn tier(&self) -> Tier {
        Tier::Batch
    }

    fn run(&self, ctx: &JobContext) -> Result<String, JobError> {
        let (source_path, content_hash, cache_root) = {
            let guard = self
                .library_slot
                .lock()
                .map_err(|_| JobError::Failed("library mutex poisoned".into()))?;
            let lib = guard
                .as_ref()
                .ok_or_else(|| JobError::Failed("no project is open".into()))?;
            let row = lib
                .sheet_by_id(&self.sheet_id)?
                .ok_or_else(|| JobError::Failed("sheet not found".into()))?;
            let root = cache_dir_for(lib.path());
            (row.source_path, row.content_hash, root)
        };
        let bytes = std::fs::read(&source_path)
            .map_err(|e| JobError::Failed(format!("read {source_path}: {e}")))?;
        let store = CacheStore::new(cache_root);
        let opts = BatchOptions {
            preset: self.preset,
            ..BatchOptions::default()
        };
        let summary = vectorize_sheet_batch(
            &bytes,
            &store,
            &self.library_slot,
            Some(SheetRef {
                id: self.sheet_id,
                content_hash,
            }),
            &opts,
            ctx.token(),
            &|done, total| ctx.progress(done, total, "vectorizing"),
        )
        .map_err(|e| match e {
            BatchError::Cancelled => JobError::Cancelled,
            other => JobError::Failed(other.to_string()),
        })?;
        let peak_mb = summary.peak_rss_bytes as f64 / (1024.0 * 1024.0);
        Ok(format!(
            "vectorized {} icons · {} cached · {} failed · peak RSS {peak_mb:.0} MB",
            summary.ok, summary.cache_hits, summary.failed
        ))
    }
}

impl Job for ImportJob {
    fn name(&self) -> &str {
        "Import folder"
    }

    fn tier(&self) -> Tier {
        Tier::Batch
    }

    fn run(&self, ctx: &JobContext) -> Result<String, JobError> {
        let mut guard = self
            .library_slot
            .lock()
            .map_err(|_| JobError::Failed("library mutex poisoned".into()))?;
        let Some(lib) = guard.as_mut() else {
            return Err(JobError::Failed("no project is open".into()));
        };
        let stats = import_folder(
            lib,
            &self.root,
            &ImportOptions::default(),
            ctx.token(),
            &mut |p| ctx.progress(u64::from(p.processed), 0, "importing"),
        )?;
        drop(guard);
        Ok(format!(
            "scanned {} · imported {} · duplicates {} · corrupt {}",
            stats.scanned, stats.imported, stats.skipped_duplicate, stats.corrupt
        ))
    }
}
