//! Cooperative cancellation primitive shared by imports, saves and jobs.
//!
//! Jobs poll [`CancellationToken::check`] at chunk boundaries; the engine and
//! the UI only ever *set* the flag. No locks, no wakeup latency beyond the
//! next checkpoint.

use std::error::Error;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Error returned when a cancellation flag was observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled;

impl fmt::Display for Cancelled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "operation cancelled")
    }
}

impl Error for Cancelled {}

/// Cheap, cloneable cancellation flag (`Arc<AtomicBool>` under the hood).
#[derive(Clone, Debug, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    /// Creates a non-cancelled token.
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    /// Sets the flag. Subsequent [`check`]s fail.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// True once [`cancel`] has been called.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    /// `Ok(())` while not cancelled, `Err(Cancelled)` afterwards. The
    /// intended use is `ctx.check()?;` at chunk boundaries.
    pub fn check(&self) -> Result<(), Cancelled> {
        if self.is_cancelled() {
            Err(Cancelled)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_starts_live_and_flips_once() {
        let t = CancellationToken::new();
        assert!(t.check().is_ok());
        assert!(!t.is_cancelled());
        t.cancel();
        assert!(t.is_cancelled());
        assert_eq!(t.check(), Err(Cancelled));
        // Idempotent.
        t.cancel();
        assert!(t.is_cancelled());
    }

    #[test]
    fn clones_share_state() {
        let a = CancellationToken::new();
        let b = a.clone();
        a.cancel();
        assert!(b.is_cancelled());
    }
}
