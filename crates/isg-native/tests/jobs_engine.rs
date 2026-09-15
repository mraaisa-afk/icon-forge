//! Job-engine integration tests: tier ordering, preemption, cancellation
//! and shutdown. Every test installs a watchdog that kills the process
//! after 30 s so an engine deadlock can never hang CI.

use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::Duration;

use isg_native::cancel::CancellationToken;
use isg_native::jobs::{Job, JobContext, JobEngine, JobError, JobEvent, JobOutcome, Tier};

struct Watchdog {
    stop: Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Watchdog {
    fn arm(name: &str) -> Self {
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop2 = Arc::clone(&stop);
        let label = name.to_string();
        let handle = std::thread::spawn(move || {
            for _ in 0..300 {
                if stop2.load(std::sync::atomic::Ordering::SeqCst) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            eprintln!("WATCHDOG: test {label} exceeded 30s — engine deadlock");
            std::process::exit(101);
        });
        Self {
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.stop
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Collects events with a matching helper.
struct EventTap {
    rx: Receiver<JobEvent>,
}

impl EventTap {
    fn new() -> (Arc<dyn Fn(JobEvent) + Send + Sync>, Self) {
        let (tx, rx) = channel();
        let tap = Self { rx };
        let sink = Arc::new(move |e: JobEvent| {
            let _ = tx.send(e);
        }) as Arc<dyn Fn(JobEvent) + Send + Sync>;
        (sink, tap)
    }

    fn next(&self, timeout: Duration) -> JobEvent {
        match self.rx.recv_timeout(timeout) {
            Ok(e) => e,
            Err(RecvTimeoutError::Timeout) => {
                eprintln!("WATCHDOG: event timeout in next()");
                std::process::exit(102);
            }
            Err(RecvTimeoutError::Disconnected) => {
                eprintln!("WATCHDOG: sink channel disconnected in next()");
                std::process::exit(103);
            }
        }
    }

    fn try_next(&self) -> Option<JobEvent> {
        self.rx.try_recv().ok()
    }
}

/// Spins until cancelled (or fails after the step cap so a missing
/// preemption cannot wedge the engine forever — the cap must be large
/// enough that the test's preemption window is hit first).
struct Spinner {
    name: String,
    tier: Tier,
    steps: u64,
}

impl Job for Spinner {
    fn name(&self) -> &str {
        &self.name
    }
    fn tier(&self) -> Tier {
        self.tier
    }
    fn run(&self, ctx: &JobContext) -> Result<String, JobError> {
        // Throttle progress to one event per ~2^20 steps for effectively-
        // endless spinners: the taps consume a fixed event budget and 5*10^8
        // events would bury the Finished/Started events under them. Jobs with
        // at most one throttle window keep exact per-step progress.
        // Cancellation is still observed every step.
        const THROTTLE: u64 = 1_048_576;
        for i in 0..self.steps {
            if self.steps <= THROTTLE || i.is_multiple_of(THROTTLE) {
                ctx.progress(i, self.steps, "spinning");
            }
            ctx.check()?;
        }
        Ok(format!("done after {} steps", self.steps))
    }
}

/// A job that completes instantly.
struct Instant {
    name: String,
    tier: Tier,
}

impl Job for Instant {
    fn name(&self) -> &str {
        &self.name
    }
    fn tier(&self) -> Tier {
        self.tier
    }
    fn run(&self, _ctx: &JobContext) -> Result<String, JobError> {
        Ok(format!("{} ok", self.name))
    }
}

fn instant(name: &str, tier: Tier) -> Box<dyn Job> {
    Box::new(Instant {
        name: name.to_string(),
        tier,
    })
}

#[test]
fn tiers_run_in_priority_order() {
    let _wd = Watchdog::arm("tiers_run_in_priority_order");
    let (sink, tap) = EventTap::new();
    let engine = JobEngine::new(sink);

    let t2a = engine.submit(instant("t2a", Tier::Batch));
    let t2b = engine.submit(instant("t2b", Tier::Batch));
    let t1 = engine.submit(instant("t1", Tier::Foreground));
    let t0 = engine.submit(instant("t0", Tier::Interactive));

    let mut finished_order: Vec<(u64, String)> = Vec::new();
    for _ in 0..4 {
        loop {
            match tap.next(Duration::from_secs(10)) {
                JobEvent::Finished { id, outcome } => {
                    let msg = match outcome {
                        JobOutcome::Succeeded(m) => m,
                        other => panic!("unexpected outcome: {other:?}"),
                    };
                    finished_order.push((id.0, msg));
                    break;
                }
                _ => {}
            }
        }
    }
    let names: Vec<&str> = finished_order
        .iter()
        .map(|(_, m)| m.split(' ').next().unwrap())
        .collect();
    assert_eq!(names, vec!["t0", "t1", "t2a", "t2b"], "tier order T0<T1<T2");

    // All ids distinct.
    let mut ids: Vec<u64> = finished_order.iter().map(|(id, _)| *id).collect();
    ids.sort_unstable();
    let mut expected = vec![t0.0, t1.0, t2a.0, t2b.0];
    expected.sort_unstable();
    assert_eq!(ids, expected);
    assert!(!engine.is_pending(t0));
    engine.shutdown();
}

#[test]
fn t0_preempts_running_t2_and_batch_resumes() {
    let _wd = Watchdog::arm("t0_preempts_running_t2_and_batch_resumes");
    let (sink, tap) = EventTap::new();
    let engine = JobEngine::new(sink);

    let t2 = engine.submit(Box::new(Spinner {
        name: "batch-spin".into(),
        tier: Tier::Batch,
        steps: 500_000_000, // effectively endless until preempted
    }));

    // Wait until the batch job actually started.
    loop {
        match tap.next(Duration::from_secs(10)) {
            JobEvent::Started { id, .. } if id == t2 => break,
            JobEvent::Progress { .. } => {}
            other => panic!("unexpected event: {other:?}"),
        }
    }

    let t0 = engine.submit(instant("urgent", Tier::Interactive));

    // The urgent job must complete while the batch job is suspended.
    let mut t0_done = false;
    let mut t2_preempted = false;
    let mut t2_resumed = false;
    for _ in 0..8 {
        match tap.next(Duration::from_secs(10)) {
            JobEvent::Started { id, .. } if id == t0 => {}
            JobEvent::Finished { id, outcome } if id == t0 => {
                assert!(matches!(outcome, JobOutcome::Succeeded(_)));
                t0_done = true;
                assert!(
                    t2_preempted,
                    "T2 suspension must be observable before T0 completes"
                );
            }
            JobEvent::Finished { id, outcome } if id == t2 && !t2_preempted => {
                assert_eq!(outcome, JobOutcome::Preempted, "batch job was preempted");
                t2_preempted = true;
            }
            JobEvent::Started { id, .. } if id == t2 && t2_preempted => {
                t2_resumed = true;
                assert!(t0_done, "T2 must not resume before T0 completed");
                // End the resumed run deterministically.
                assert!(engine.cancel(t2));
            }
            JobEvent::Finished { id, outcome } if id == t2 && t2_resumed => {
                assert_eq!(outcome, JobOutcome::Cancelled);
                break;
            }
            JobEvent::Progress { .. } => {}
            other => panic!("unexpected event: {other:?}"),
        }
    }
    assert!(t0_done, "T0 completed");
    assert!(t2_preempted, "T2 preempted");
    assert!(t2_resumed, "T2 resumed after T0");
    engine.shutdown();
}

#[test]
fn cancel_queued_job_never_runs() {
    let _wd = Watchdog::arm("cancel_queued_job_never_runs");
    let (sink, tap) = EventTap::new();
    let engine = JobEngine::new(sink);

    // Occupy the worker with a spinner.
    let t2spin = engine.submit(Box::new(Spinner {
        name: "spin".into(),
        tier: Tier::Batch,
        steps: 500_000_000,
    }));
    loop {
        match tap.next(Duration::from_secs(10)) {
            JobEvent::Started { id, .. } if id == t2spin => break,
            _ => {}
        }
    }
    // Queue a second batch job, then cancel it before it can start.
    let doomed = engine.submit(instant("doomed", Tier::Batch));
    assert!(engine.is_pending(doomed));
    assert!(engine.cancel(doomed), "queued job exists");

    // The only Finished for `doomed` must be Cancelled.
    let mut saw_cancel = false;
    for _ in 0..32 {
        match tap.next(Duration::from_secs(10)) {
            JobEvent::Finished { id, outcome } if id == doomed => {
                assert_eq!(outcome, JobOutcome::Cancelled);
                saw_cancel = true;
                break;
            }
            JobEvent::Finished { id, .. } if id == t2spin => panic!("spinner finished early"),
            _ => {}
        }
    }
    assert!(saw_cancel, "cancellation event delivered");
    engine.shutdown();
}

#[test]
fn cancel_running_job_reports_cancelled_not_preempted() {
    let _wd = Watchdog::arm("cancel_running_job_reports_cancelled_not_preempted");
    let (sink, tap) = EventTap::new();
    let engine = JobEngine::new(sink);

    let spin = engine.submit(Box::new(Spinner {
        name: "spin".into(),
        tier: Tier::Batch,
        steps: 500_000_000,
    }));
    loop {
        match tap.next(Duration::from_secs(10)) {
            JobEvent::Started { id, .. } if id == spin => break,
            _ => {}
        }
    }
    assert!(engine.cancel(spin));
    loop {
        match tap.next(Duration::from_secs(10)) {
            JobEvent::Finished { id, outcome } if id == spin => {
                assert_eq!(outcome, JobOutcome::Cancelled, "user cancel != preempted");
                break;
            }
            _ => {}
        }
    }
    engine.shutdown();
}

#[test]
fn shutdown_cancels_running_job_and_joins() {
    let _wd = Watchdog::arm("shutdown_cancels_running_job_and_joins");
    let (sink, _tap) = EventTap::new();
    let engine = JobEngine::new(sink);
    let spin = engine.submit(Box::new(Spinner {
        name: "spin".into(),
        tier: Tier::Batch,
        steps: 500_000_000,
    }));
    assert!(engine.is_pending(spin));
    // Drop the engine: shutdown must cancel the spinner and join the worker.
    drop(engine);

    // Shared token machinery sanity (exercised here as a unit).
    let tok = CancellationToken::new();
    let tok2 = tok.clone();
    tok.cancel();
    assert!(tok2.check().is_err());
}

#[test]
fn progress_events_flow_through_sink() {
    let _wd = Watchdog::arm("progress_events_flow_through_sink");
    let (sink, tap) = EventTap::new();
    let engine = JobEngine::new(sink);
    let id = engine.submit(Box::new(Spinner {
        name: "tick".into(),
        tier: Tier::Batch,
        steps: 5,
    }));
    let mut progress_seen = 0u64;
    loop {
        match tap.next(Duration::from_secs(10)) {
            JobEvent::Progress { id: p, done, .. } if p == id => {
                assert_eq!(done, progress_seen, "progress is sequential");
                progress_seen += 1;
            }
            JobEvent::Finished { id: f, outcome } if f == id => {
                assert_eq!(outcome, JobOutcome::Succeeded("done after 5 steps".into()));
                break;
            }
            _ => {}
        }
    }
    assert_eq!(progress_seen, 5);
    engine.shutdown();
}

#[test]
fn tap_try_next_is_nonblocking() {
    let _wd = Watchdog::arm("tap_try_next_is_nonblocking");
    let (sink, tap) = EventTap::new();
    assert!(tap.try_next().is_none(), "no events before engine exists");
    let engine = JobEngine::new(sink);
    let id = engine.submit(instant("quick", Tier::Interactive));
    loop {
        match tap.next(Duration::from_secs(10)) {
            JobEvent::Finished { id: f, .. } if f == id => break,
            _ => {}
        }
    }
    engine.shutdown();
    // Event channel drained at most a bounded amount — drain what's left.
    while let Some(_e) = tap.try_next() {}
}
