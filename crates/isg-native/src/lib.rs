//! # isg-native — Icon Forge native glue
//!
//! Everything that is deliberately **not** in [`isg_core`](https://docs.rs/isg-core):
//! file systems, SQLite, hashing, and the T0/T1/T2 job engine. This crate is
//! linked into the Tauri host process only (ARCHITECTURE.md §2, §5) and is
//! never compiled for wasm — that boundary belongs to `isg-core`.
//!
//! Modules:
//!
//! * [`cancel`] — cooperative [`CancellationToken`]
//! * [`db`] — the SQLite library (WAL), schema per ARCHITECTURE.md §4
//! * [`import`] — streaming folder import (blake3-deduplicated, cancellable)
//! * [`cache`] — content-addressed blake3/zstd payload cache (§3.3 stage 8)
//! * [`project`] — atomic `.isgproj` save/open (kill-safe by construction)
//! * [`jobs`] — T0/T1/T2 job engine with cancellation and preemption
#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod cache;
pub mod cancel;
pub mod db;
pub mod import;
pub mod jobs;
pub mod project;

/// Error type shared by the native glue modules.
#[derive(Debug)]
pub enum IsgError {
    /// SQLite failure.
    Db(rusqlite::Error),
    /// File-system / OS failure.
    Io(std::io::Error),
    /// zstd compression failure.
    Zstd(std::io::Error),
    /// A project/library file failed validation on open.
    Corrupt(String),
    /// The operation was cancelled through its token.
    Cancelled(cancel::Cancelled),
}

impl std::fmt::Display for IsgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IsgError::Db(e) => write!(f, "database error: {e}"),
            IsgError::Io(e) => write!(f, "io error: {e}"),
            IsgError::Zstd(e) => write!(f, "compression error: {e}"),
            IsgError::Corrupt(msg) => write!(f, "corrupt project: {msg}"),
            IsgError::Cancelled(e) => write!(f, "cancelled: {e}"),
        }
    }
}

impl std::error::Error for IsgError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            IsgError::Db(e) => Some(e),
            IsgError::Io(e) | IsgError::Zstd(e) => Some(e),
            IsgError::Cancelled(e) => Some(e),
            IsgError::Corrupt(_) => None,
        }
    }
}

impl From<rusqlite::Error> for IsgError {
    fn from(e: rusqlite::Error) -> Self {
        IsgError::Db(e)
    }
}

impl From<std::io::Error> for IsgError {
    fn from(e: std::io::Error) -> Self {
        IsgError::Io(e)
    }
}

impl From<cancel::Cancelled> for IsgError {
    fn from(e: cancel::Cancelled) -> Self {
        IsgError::Cancelled(e)
    }
}

/// Convenient result alias for the native glue.
pub type Result<T> = std::result::Result<T, IsgError>;
