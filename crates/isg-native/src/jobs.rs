//! T0/T1/T2 job engine with cooperative cancellation and preemption
//! (ARCHITECTURE.md §2: "Job Engine (T0/T1/T2 tasks, cancellation,
//! preemption)").
//!
//! Semantics (deliberately simple, Phase 1):
//!
//! * **One worker runs jobs serially.** Inside a job, bulk work may fan out
//!   to `rayon`; the engine never runs two jobs at once, which makes
//!   ordering, preemption and cancellation deterministic and testable.
//! * **Tiers**: `T0` interactive < `T1` foreground < `T2` batch. The worker
//!   always picks the lowest-numbered non-empty tier.
//! * **Preemption**: submitting a job whose tier is strictly higher than the
//!   running job's tier cancels the running job's token; at its next
//!   checkpoint the job returns `Err(Cancelled)`, the engine requeues it at
//!   the *front* of its tier and starts the higher-tier job.
//! * **Cancellation**: engine- or user-initiated token cancel; a cancelled
//!   job is dropped (not requeued) unless the cancel was engine preemption.
//!
//! Events (`Started` / `Progress` / `Finished`) are delivered on the
//! engine's sink thread — the worker thread, *never* while holding the
//! engine lock, so sinks may re-enter `submit`/`cancel`.

use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use crate::cancel::CancellationToken;

/// Job priority tier. Lower value = higher priority.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    /// T0 — interactive (UI-blocking if it waits; preempts everything).
    Interactive,
    /// T1 — foreground (visible work the user is waiting on).
    Foreground,
    /// T2 — batch (background: imports, bulk vectorization).
    Batch,
}

impl Tier {
    /// Queue index (0..=2).
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Tier::Interactive => 0,
            Tier::Foreground => 1,
            Tier::Batch => 2,
        }
    }
}

impl fmt::Display for Tier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Tier::Interactive => write!(f, "T0"),
            Tier::Foreground => write!(f, "T1"),
            Tier::Batch => write!(f, "T2"),
        }
    }
}

/// Engine-assigned job identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct JobId(
    /// The raw id (monotonic, starts at 1).
    pub u64,
);

impl fmt::Display for JobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// Failure modes a job can report to the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobError {
    /// The job observed a cancelled token (engine preemption or user cancel
    /// — the engine distinguishes the two).
    Cancelled,
    /// The job failed with a human-readable detail string.
    Failed(String),
}

impl fmt::Display for JobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JobError::Cancelled => write!(f, "cancelled"),
            JobError::Failed(m) => write!(f, "failed: {m}"),
        }
    }
}

impl std::error::Error for JobError {}

impl From<crate::IsgError> for JobError {
    fn from(e: crate::IsgError) -> Self {
        if matches!(e, crate::IsgError::Cancelled(_)) {
            return JobError::Cancelled;
        }
        JobError::Failed(e.to_string())
    }
}

impl From<crate::cancel::Cancelled> for JobError {
    fn from(_: crate::cancel::Cancelled) -> Self {
        JobError::Cancelled
    }
}

/// A unit of work. `run` must be cooperative: call
/// [`JobContext::check`] at least every few hundred milliseconds so
/// cancellation and preemption stay responsive.
pub trait Job: Send {
    /// Human-readable job name (events, logs, UI).
    fn name(&self) -> &str;
    /// Priority tier.
    fn tier(&self) -> Tier;
    /// Executes the job, returning a completion message on success.
    fn run(&self, ctx: &JobContext) -> Result<String, JobError>;
}

/// Per-run context handed to [`Job::run`].
pub struct JobContext {
    token: CancellationToken,
    sink: Arc<dyn Fn(JobEvent) + Send + Sync>,
    id: JobId,
}

impl JobContext {
    /// `Ok(())` while the job may continue; `Err(Cancelled)` once the token
    /// fired. Call this at every chunk boundary.
    pub fn check(&self) -> Result<(), crate::cancel::Cancelled> {
        self.token.check()
    }

    /// The live cancellation flag of this run — clone it to hand long-lived
    /// workers (imports, batch vectorization) cooperative cancellation.
    #[must_use]
    pub fn token(&self) -> &CancellationToken {
        &self.token
    }

    /// True when the token has fired.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    /// The engine-assigned id of the running job.
    #[must_use]
    pub const fn job_id(&self) -> JobId {
        self.id
    }

    /// Reports progress (also forwarded to the engine sink as a
    /// [`JobEvent::Progress`]).
    pub fn progress(&self, done: u64, total: u64, message: &str) {
        (self.sink)(JobEvent::Progress {
            id: self.id,
            done,
            total,
            message: message.to_string(),
        });
    }
}

/// Terminal state of a finished job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobOutcome {
    /// Completed with a message.
    Succeeded(String),
    /// Cancelled (user or shutdown) — not requeued.
    Cancelled,
    /// Preempted by a higher tier — requeued at the front of its tier.
    Preempted,
    /// Failed with a detail message.
    Failed(String),
}

/// Engine → application event stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobEvent {
    /// A job left the queue and started executing.
    Started {
        /// Job id.
        id: JobId,
        /// Job name.
        name: String,
    },
    /// Job-reported progress.
    Progress {
        /// Job id.
        id: JobId,
        /// Completed units.
        done: u64,
        /// Total units (may be 0 when unknown).
        total: u64,
        /// Free-form message.
        message: String,
    },
    /// A job reached a terminal state (including from `cancel`).
    Finished {
        /// Job id.
        id: JobId,
        /// Terminal outcome.
        outcome: JobOutcome,
    },
}

struct QueuedJob {
    id: u64,
    name: String,
    tier: Tier,
    job: Box<dyn Job>,
}

struct Running {
    id: u64,
    tier: Tier,
    token: CancellationToken,
}

#[derive(Default)]
struct Inner {
    queues: [VecDeque<QueuedJob>; 3],
    next_id: u64,
    running: Option<Running>,
    pending_preempt: Option<u64>,
    shutting_down: bool,
}

impl Inner {
    fn highest_queued_tier(&self) -> Option<Tier> {
        for (i, q) in self.queues.iter().enumerate() {
            if !q.is_empty() {
                return Some(match i {
                    0 => Tier::Interactive,
                    1 => Tier::Foreground,
                    _ => Tier::Batch,
                });
            }
        }
        None
    }

    fn pop_highest(&mut self) -> Option<QueuedJob> {
        for q in &mut self.queues {
            if let Some(job) = q.pop_front() {
                return Some(job);
            }
        }
        None
    }
}

struct Shared {
    inner: Mutex<Inner>,
    cv: Condvar,
    sink: Arc<dyn Fn(JobEvent) + Send + Sync>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

/// Handle to the running engine. Dropping any handle — or calling
/// [`JobEngine::shutdown`] explicitly — cancels everything and joins the
/// worker (Phase 1 keeps a single long-lived engine, so clone-drop equals
/// shutdown).
#[derive(Clone)]
pub struct JobEngine {
    shared: Arc<Shared>,
}

impl fmt::Debug for JobEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let g = self.shared.inner.lock().unwrap();
        write!(
            f,
            "JobEngine {{ queued: [{}, {}, {}], running: {:?}, shutting_down: {} }}",
            g.queues[0].len(),
            g.queues[1].len(),
            g.queues[2].len(),
            g.running.as_ref().map(|r| r.id),
            g.shutting_down
        )
    }
}

impl JobEngine {
    /// Starts the engine with one worker and the given event sink.
    pub fn new(sink: Arc<dyn Fn(JobEvent) + Send + Sync>) -> Self {
        let shared = Arc::new(Shared {
            inner: Mutex::new(Inner {
                queues: [VecDeque::new(), VecDeque::new(), VecDeque::new()],
                next_id: 1,
                running: None,
                pending_preempt: None,
                shutting_down: false,
            }),
            cv: Condvar::new(),
            sink,
            worker: Mutex::new(None),
        });
        let worker_shared = Arc::clone(&shared);
        let handle = std::thread::Builder::new()
            .name("isg-job-engine".into())
            .spawn(move || worker_loop(worker_shared))
            .expect("failed to spawn job engine worker");
        *shared.worker.lock().unwrap() = Some(handle);
        Self { shared }
    }

    /// Submits a job and returns its engine-assigned id. If a strictly
    /// higher-priority job than the currently running one is submitted, the
    /// running job is flagged for preemption at its next checkpoint.
    pub fn submit(&self, job: Box<dyn Job>) -> JobId {
        let mut g = self.shared.inner.lock().unwrap();
        g.next_id += 1;
        let id = g.next_id;
        let tier = job.tier();
        g.queues[tier.index()].push_back(QueuedJob {
            id,
            name: job.name().to_string(),
            tier,
            job,
        });
        // Collect the preemption decision under shared borrows first: `g`
        // is a guard deref, so field writes conflict with any live borrow
        // of another field.
        let preempt = match &g.running {
            Some(run) if run.tier > tier && g.pending_preempt != Some(run.id) => {
                let id = run.id;
                run.token.cancel();
                Some(id)
            }
            _ => None,
        };
        if let Some(preempted) = preempt {
            g.pending_preempt = Some(preempted);
        }
        self.shared.cv.notify_all();
        JobId(id)
    }

    /// Cancels a queued or running job. Returns `false` if the id is
    /// unknown (already finished). Emits `Finished { Cancelled }` for queued
    /// jobs; running jobs emit `Finished` when they reach their next
    /// checkpoint.
    pub fn cancel(&self, id: JobId) -> bool {
        let mut events: Vec<JobEvent> = Vec::new();
        let found = {
            let mut g = self.shared.inner.lock().unwrap();
            let mut found = false;
            for q in &mut g.queues {
                if let Some(pos) = q.iter().position(|j| j.id == id.0) {
                    q.remove(pos);
                    found = true;
                    events.push(JobEvent::Finished {
                        id,
                        outcome: JobOutcome::Cancelled,
                    });
                    break;
                }
            }
            if !found {
                if let Some(run) = &g.running {
                    if run.id == id.0 {
                        run.token.cancel();
                        found = true;
                    }
                }
            }
            found
        };
        for e in events {
            (self.shared.sink)(e);
        }
        found
    }

    /// True when a job with this id is queued or running.
    #[must_use]
    pub fn is_pending(&self, id: JobId) -> bool {
        let g = self.shared.inner.lock().unwrap();
        if g.running.as_ref().is_some_and(|r| r.id == id.0) {
            return true;
        }
        g.queues.iter().any(|q| q.iter().any(|j| j.id == id.0))
    }

    /// Cancels everything (running and queued) and joins the worker.
    /// Also invoked by `Drop`. Jobs must be cooperative for this to return
    /// promptly.
    pub fn shutdown(&self) {
        {
            let mut g = self.shared.inner.lock().unwrap();
            g.shutting_down = true;
            g.pending_preempt = None; // every Cancelled now means plain cancel
            if let Some(run) = &g.running {
                run.token.cancel();
            }
            g.queues = [VecDeque::new(), VecDeque::new(), VecDeque::new()];
            self.shared.cv.notify_all();
        }
        let handle = self.shared.worker.lock().unwrap().take();
        if let Some(h) = handle {
            let _ = h.join();
        }
    }
}

impl Drop for JobEngine {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn worker_loop(shared: Arc<Shared>) {
    loop {
        let claimed: Option<(QueuedJob, CancellationToken)> = {
            let mut g = shared.inner.lock().unwrap();
            loop {
                // Preemption: strictly higher tier queued while a lower tier
                // runs → cancel the running job's token. As in `submit`, the
                // decision is collected under shared borrows, the write
                // happens after.
                let preempt = match (&g.running, g.highest_queued_tier()) {
                    (Some(run), Some(tier)) if tier < run.tier && g.pending_preempt != Some(run.id) => {
                        let id = run.id;
                        run.token.cancel();
                        Some(id)
                    }
                    _ => None,
                };
                if let Some(preempted) = preempt {
                    g.pending_preempt = Some(preempted);
                }
                if g.running.is_none() {
                    if let Some(q) = g.pop_highest() {
                        let token = CancellationToken::new();
                        g.running = Some(Running {
                            id: q.id,
                            tier: q.tier,
                            token: token.clone(),
                        });
                        break Some((q, token));
                    }
                }
                if g.shutting_down && g.running.is_none() {
                    break None;
                }
                g = shared.cv.wait(g).unwrap();
            }
        };
        let Some((q, token)) = claimed else {
            return; // shutting down, idle
        };

        (shared.sink)(JobEvent::Started {
            id: JobId(q.id),
            name: q.name.clone(),
        });
        let ctx = JobContext {
            token,
            sink: Arc::clone(&shared.sink),
            id: JobId(q.id),
        };
        let result = q.job.run(&ctx);

        let event = {
            let mut g = shared.inner.lock().unwrap();
            let outcome = match result {
                Ok(msg) => JobOutcome::Succeeded(msg),
                Err(JobError::Cancelled) => {
                    if g.pending_preempt == Some(q.id) {
                        g.pending_preempt = None;
                        g.queues[q.tier.index()].push_front(QueuedJob {
                            id: q.id,
                            name: q.name.clone(),
                            tier: q.tier,
                            job: q.job,
                        });
                        JobOutcome::Preempted
                    } else {
                        JobOutcome::Cancelled
                    }
                }
                Err(JobError::Failed(m)) => JobOutcome::Failed(m),
            };
            g.running = None;
            g.queues[q.tier.index()].shrink_to_fit();
            shared.cv.notify_all();
            JobEvent::Finished {
                id: JobId(q.id),
                outcome,
            }
        };
        (shared.sink)(event);
    }
}
