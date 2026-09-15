//! Managed application state.

use std::sync::{Arc, Mutex};

use isg_native::db::Library;

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
}

impl App {
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
