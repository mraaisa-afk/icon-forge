//! IPC commands — the webview-facing surface. Thin validation +
//! [`isg_native`] forwarding; every long operation goes through the job
//! engine instead of blocking a command.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{Builder, State};

use isg_core::{Bbox, TracePreset};
use isg_native::cache::CacheStore;
use isg_native::db::Library;
use isg_native::jobs::{JobEngine, JobId};
use isg_native::pipeline::{
    cached_vectorize, segment, SegParams, VectorizeError,
};

use crate::jobs::{cache_dir_for, ImportJob, VectorizeSheetJob};
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

impl From<VectorizeError> for CmdError {
    fn from(e: VectorizeError) -> Self {
        CmdError {
            message: e.to_string(),
        }
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

/// Decodes a 16-byte id from its 32-char hex form.
fn parse_hex16(s: &str) -> Option<[u8; 16]> {
    let b = s.as_bytes();
    if b.len() != 32 {
        return None;
    }
    let hex = |c: u8| -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    };
    let mut out = [0u8; 16];
    for (i, pair) in b.chunks_exact(2).enumerate() {
        out[i] = (hex(pair[0])? << 4) | hex(pair[1])?;
    }
    Some(out)
}

/// §3.3-⑤ doc name → frozen preset.
fn preset_from_name(name: &str) -> Option<TracePreset> {
    match name {
        "mono-fast" => Some(TracePreset::Draft),
        "mono-clean" => Some(TracePreset::Wireframe),
        "scan" => Some(TracePreset::Lineart),
        "flat-8" => Some(TracePreset::Balanced),
        "flat-cutout" => Some(TracePreset::Detailed),
        "detailed" => Some(TracePreset::HighFidelity),
        "pixel-art" => Some(TracePreset::Pixel),
        _ => None,
    }
}

/// Stage ⑧ quality metrics for one icon.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoreDto {
    /// Mean absolute ink-plane error.
    pub mae: f32,
    /// Block SSIM (8×8 windows).
    pub ssim: f32,
    /// Ink IoU (alpha ≥ 128) after centroid alignment.
    pub iou: f32,
    /// `0.5·SSIM + 0.3·(1−MAE) + 0.2·IoU`.
    pub composite: f32,
}

/// A single icon's final SVG + score (review preview / W5 comparator).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IconSvgDto {
    /// Validated SVG document.
    pub svg: String,
    /// True when served from the stage ⑧ cache.
    pub cached: bool,
    /// Quality score against the source crop.
    pub score: ScoreDto,
}

#[derive(Debug, Deserialize)]
pub struct VectorizeSheetRequest {
    /// Hex-encoded 16-byte sheet id.
    pub sheet_id: String,
    /// §3.3-⑤ preset doc name (e.g. `mono-clean`, `flat-8`).
    pub preset: String,
}

/// Submits T2 bulk vectorization of one sheet; progress flows through the
/// usual `job://event` stream, results land in the icons table + cache.
#[tauri::command]
pub fn vectorize_sheet_submit(
    state: State<'_, App>,
    engine: State<'_, JobEngine>,
    req: VectorizeSheetRequest,
) -> CmdResult<u64> {
    let id = parse_hex16(&req.sheet_id).ok_or_else(|| CmdError {
        message: format!("bad sheet id: {}", req.sheet_id),
    })?;
    let preset = preset_from_name(&req.preset).ok_or_else(|| CmdError {
        message: format!("unknown preset: {}", req.preset),
    })?;
    {
        let guard = state.lock()?;
        let Some(lib) = guard.as_ref() else {
            return Err(CmdError {
                message: "no project is open".into(),
            });
        };
        if lib.sheet_by_id(&id)?.is_none() {
            return Err(CmdError {
                message: "sheet not found".into(),
            });
        }
    }
    let job_id = engine.submit(Box::new(VectorizeSheetJob {
        sheet_id: id,
        preset,
        library_slot: state.slot(),
    }));
    Ok(job_id.0)
}

/// Vectorizes (or cache-serves) one icon and returns its SVG + score.
#[tauri::command]
pub fn vectorize_icon(
    state: State<'_, App>,
    sheet_id: String,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    preset: String,
) -> CmdResult<IconSvgDto> {
    let id = parse_hex16(&sheet_id).ok_or_else(|| CmdError {
        message: format!("bad sheet id: {sheet_id}"),
    })?;
    let preset = preset_from_name(&preset).ok_or_else(|| CmdError {
        message: format!("unknown preset: {preset}"),
    })?;
    let bbox = Bbox::new(x, y, w, h).ok_or_else(|| CmdError {
        message: "empty or invalid bbox".into(),
    })?;
    let guard = state.lock()?;
    let Some(lib) = guard.as_ref() else {
        return Err(CmdError {
            message: "no project is open".into(),
        });
    };
    let row = lib
        .sheet_by_id(&id)?
        .ok_or_else(|| CmdError {
            message: "sheet not found".into(),
        })?;
    let store = CacheStore::new(cache_dir_for(lib.path()));
    let bytes = std::fs::read(&row.source_path).map_err(|e| CmdError {
        message: format!("read {}: {e}", row.source_path),
    })?;
    let seg_params = SegParams::default();
    let out = segment(&bytes, 4096, &seg_params)?;
    let (icon, cached) = cached_vectorize(
        &store,
        lib,
        &out.sheet,
        bbox,
        &out.background,
        preset,
        &seg_params.to_cache_string(),
    )?;
    Ok(IconSvgDto {
        svg: icon.svg,
        cached,
        score: ScoreDto {
            mae: icon.score.mae,
            ssim: icon.score.ssim,
            iou: icon.score.iou,
            composite: icon.score.composite,
        },
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
        vectorize_sheet_submit,
        vectorize_icon,
    ])
}
