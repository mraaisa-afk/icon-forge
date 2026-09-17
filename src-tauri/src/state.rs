//! Managed application state.

use std::sync::{Arc, Mutex};

use isg_native::db::Library;
use isg_native::pipeline::GroupingSession;

/// The library handle shared by all commands and by long-running jobs.
///
/// The mutex lives behind an `Arc` so an [`crate::jobs::ImportJob`] can hold
/// the *same* lock the commands use (the engine runs on its own thread; the
/// WAL makes concurrent read commands safe while an import holds the lock).
///
/// The engine itself is managed separately (`State<JobEngine>`) because it
/// is spawned in `setup`, where the `AppHandle` for event forwarding exists.
#[derive(Clone, Default)]
pub struct App {
    library_slot: Arc<Mutex<Option<Library>>>,
    grouping_slot: Arc<Mutex<Option<GroupingSession>>>,
}

impl App {
    /// Sheets kept segmented (mask-cached) for the live grouping UI. Four is
    /// enough to move back and forth between a sheet and its neighbours
    /// without re-decoding; anything larger only holds memory (W12).
    pub const MASK_CACHE_SHEETS: usize = 4;

    /// The grouping session slot. Commands insert a session on first use, so
    /// no mask cache (and no `Arc` bookkeeping) exists until the UI groups
    /// something.
    pub fn grouping_slot(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, Option<GroupingSession>>, crate::commands::CmdError> {
        self.grouping_slot
            .lock()
            .map_err(|_| crate::commands::CmdError {
                message: "grouping mutex poisoned".into(),
            })
    }

    /// The shared library slot.
    #[must_use]
    pub fn slot(&self) -> Arc<Mutex<Option<Library>>> {
        Arc::clone(&self.library_slot)
    }

    /// Locked view of the library (poisoning surfaced as an error string).
    pub fn lock(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, Option<Library>>, crate::commands::CmdError> {
        self.library_slot
            .lock()
            .map_err(|_| crate::commands::CmdError {
                message: "library mutex poisoned".into(),
            })
    }
}
