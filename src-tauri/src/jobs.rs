//! Job-engine wiring: spawns the T0/T1/T2 [`JobEngine`] with a sink that
//! forwards [`JobEvent`]s to the webview as `job://event` payloads, and
//! provides the import job implementation.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use isg_native::db::Library;
use isg_native::import::{import_folder, ImportOptions};
use isg_native::jobs::{Job, JobContext, JobEngine, JobError, JobEvent, JobOutcome, Tier};

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
