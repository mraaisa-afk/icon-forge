//! IPC commands — the webview-facing surface. Thin validation +
//! [`isg_native`] forwarding; every long operation goes through the job
//! engine instead of blocking a command.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{Builder, State};

use isg_native::jobs::{JobEngine, JobId};

use crate::jobs::ImportJob;
use crate::state::App;

/// Serializable error for the webview (`Result<T, CmdError>` in JS).
#[derive(Debug, Serialize)]
pub struct CmdError {
    /// Human-readable message.
    pub message: String,
}

impl From<isg_native::IsgError> for CmdError {
    fn from(e: isg_native::IsgError) -> Self {
        CmdError {
            message: e.to_string(),
        }
    }
}

fn poisoned() -> CmdError {
    CmdError {
        message: "library mutex poisoned".into(),
    }
}

type CmdResult<T> = Result<T, CmdError>;

/// Library summary returned by open/create/save.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectInfo {
    /// Absolute path of the project file.
    pub path: String,
    /// Number of sheet rows.
    pub sheet_count: u64,
    /// Number of icon rows.
    pub icon_count: u64,
}

/// A library-grid row.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SheetDto {
    /// Hex-encoded 16-byte id.
    pub id: String,
    /// Source path.
    pub source_path: String,
    /// blake3 hex digest.
    pub content_hash: String,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Import timestamp (epoch millis as text).
    pub imported_at: String,
}

/// Library totals for the header bar.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatsDto {
    /// Sheet rows.
    pub sheets: u64,
    /// Icon rows.
    pub icons: u64,
}

#[derive(Debug, Deserialize)]
pub struct OpenRequest {
    /// Absolute path to an `.isgproj` (or bare `.db`) file.
    pub path: String,
}

fn info(path: &str, lib: &Library) -> CmdResult<ProjectInfo> {
    Ok(ProjectInfo {
        path: path.to_string(),
        sheet_count: lib.sheet_count()?,
        icon_count: lib.icon_count()?,
    })
}

/// Opens (running migrations as needed) an existing project file.
#[tauri::command]
pub fn project_open(state: State<'_, App>, req: OpenRequest) -> CmdResult<ProjectInfo> {
    let path = PathBuf::from(&req.path);
    let lib = Library::open(&path)?;
    lib.verify_integrity()?;
    let out = info(&req.path, &lib)?;
    *state.lock()? = Some(lib);
    Ok(out)
}

/// Creates a new project file (fails when it already exists).
#[tauri::command]
pub fn project_create(state: State<'_, App>, req: OpenRequest) -> CmdResult<ProjectInfo> {
    let path = PathBuf::from(&req.path);
    if path.exists() {
        return Err(CmdError {
            message: format!("{} already exists", req.path),
        });
    }
    let lib = Library::open(&path)?;
    let out = info(&req.path, &lib)?;
    *state.lock()? = Some(lib);
    Ok(out)
}

/// Closes the project (WAL sidecars keep uncommitted state for SQLite to
/// recover on next open).
#[tauri::command]
pub fn project_close(state: State<'_, App>) -> CmdResult<()> {
    *state.lock()? = None;
    Ok(())
}

/// Saves an atomic snapshot in place (the kill-safe save).
#[tauri::command]
pub fn project_save(state: State<'_, App>) -> CmdResult<ProjectInfo> {
    let mut guard = state.lock()?;
    let Some(lib) = guard.as_mut() else {
        return Err(CmdError {
            message: "no project is open".into(),
        });
    };
    lib.save_in_place()?;
    let path = lib
        .path()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    info(&path, lib)
}

/// Submits a folder import job (T2). Returns the job id; progress arrives
/// as `job://event` payloads.
#[tauri::command]
pub fn import_submit(
    state: State<'_, App>,
    engine: State<'_, JobEngine>,
    root: String,
) -> CmdResult<u64> {
    let path = PathBuf::from(&root);
    if !path.is_dir() {
        return Err(CmdError {
            message: format!("not a directory: {root}"),
        });
    }
    {
        let guard = state.lock()?;
        if guard.is_none() {
            return Err(CmdError {
                message: "no project is open".into(),
            });
        }
    }
    let id = engine.submit(Box::new(ImportJob {
        root: path,
        library_slot: state.slot(),
    }));
    Ok(id.0)
}

/// Cancels a queued/running job. Returns false if it already finished.
#[tauri::command]
pub fn job_cancel(engine: State<'_, JobEngine>, id: u64) -> bool {
    engine.cancel(JobId(id))
}

/// One page of the virtualized library grid.
#[tauri::command]
pub fn library_sheets(state: State<'_, App>, offset: u64, limit: u32) -> CmdResult<Vec<SheetDto>> {
    let guard = state.lock()?;
    let Some(lib) = guard.as_ref() else {
        return Err(CmdError {
            message: "no project is open".into(),
        });
    };
    let rows = lib.list_sheets(offset, limit)?;
    Ok(rows
        .into_iter()
        .map(|r| SheetDto {
            id: r.id.iter().map(|b| format!("{b:02x}")).collect(),
            source_path: r.source_path,
            content_hash: r.content_hash,
            width: r.width,
            height: r.height,
            imported_at: r.imported_at,
        })
        .collect())
}

/// Library totals.
#[tauri::command]
pub fn library_stats(state: State<'_, App>) -> CmdResult<StatsDto> {
    let guard = state.lock()?;
    let Some(lib) = guard.as_ref() else {
        return Err(CmdError {
            message: "no project is open".into(),
        });
    };
    Ok(StatsDto {
        sheets: lib.sheet_count()?,
        icons: lib.icon_count()?,
    })
}

/// Registers all commands on the builder.
pub fn register(builder: Builder<tauri::Wry>) -> Builder<tauri::Wry> {
    builder.invoke_handler(tauri::generate_handler![
        project_open,
        project_create,
        project_close,
        project_save,
        import_submit,
        job_cancel,
        library_sheets,
        library_stats,
    ])
}
