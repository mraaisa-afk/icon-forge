//! IPC commands — the webview-facing surface. Thin validation +
//! [`isg_native`] forwarding; every long operation goes through the job
//! engine instead of blocking a command.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{Builder, State};

use isg_core::{Bbox, TracePreset};
use isg_native::cache::CacheStore;
use isg_native::db::{IconVectorRow, Library};
use isg_native::jobs::{JobEngine, JobId};
use isg_native::pipeline::{
    cached_vectorize, mask_cached, mask_key, segment, GroupingReport, GroupingSession,
    PreviewImage, RefineParams, SegParams, SensitivityError, SensitivityParams, VectorizeError,
    WarningKind,
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
    for (i, pair) in b.as_chunks::<2>().0.iter().enumerate() {
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

/// A stored icon row (comparator grid / review list).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IconDto {
    /// 32-char hex of the 16-byte icon id.
    pub id: String,
    /// Tight bbox `(x, y, w, h)` inside the sheet.
    pub bbox: (u32, u32, u32, u32),
    /// Stage ⑧ cache key (empty when the payload is not cached).
    pub svg_key: String,
    /// §3.3-⑤ preset doc name.
    pub preset: String,
    /// Mean absolute ink-plane error.
    pub mae: f32,
    /// Block SSIM (8×8 windows).
    pub ssim: f32,
    /// Ink IoU (alpha ≥ 128).
    pub iou: f32,
}

impl From<&IconVectorRow> for IconDto {
    fn from(r: &IconVectorRow) -> Self {
        IconDto {
            id: r.id.iter().map(|b| format!("{b:02x}")).collect(),
            bbox: r.bbox,
            svg_key: r.svg_key.clone(),
            preset: r.preset.clone(),
            mae: r.mae,
            ssim: r.ssim,
            iou: r.iou,
        }
    }
}

/// Largest accepted `sheet_preview` longest side. The overlay draws into a
/// few hundred CSS pixels, so anything above this only costs bandwidth.
pub const PREVIEW_MAX_DIM: u32 = 2048;

// ─────────────────────────── Group All (W12) ───────────────────────────
//
// The "Group All" UI runs grouping *live* (unlike T2, which is a job): a cached
// mask makes a regrouping cost milliseconds, and the overlay needs the answer in
// the same frame as the click. Decode + segmentation still take ~2 s on the
// first call for a sheet — `mask_cache_hit` in the response tells the UI whether
// it waited for that, so it can say so instead of pretending to be instant.

/// The four sensitivity sliders, as the webview sends them.
///
/// The envelope and the parameter mapping live in
/// [`isg_native::pipeline::SensitivityParams`] (tested there); this is only the
/// serde shape.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SensitivityDto {
    /// F1: merge distance as a fraction of `median_h`.
    pub merge_gap_frac: f32,
    /// Rule 5: combined area ≤ this × `median_area`.
    pub merge_area_ratio: f32,
    /// F4: components below this area are speckle.
    pub noise_min_area: u32,
    /// F5: minimum lattice regularity for the grid hint.
    pub grid_regularity_min: f32,
}

impl SensitivityDto {
    /// The native knob set.
    #[must_use]
    fn native(self) -> SensitivityParams {
        SensitivityParams {
            merge_gap_frac: self.merge_gap_frac,
            merge_area_ratio: self.merge_area_ratio,
            noise_min_area: self.noise_min_area,
            grid_regularity_min: self.grid_regularity_min,
        }
    }

    /// The webview shape of a native knob set.
    #[must_use]
    fn from_native(p: SensitivityParams) -> Self {
        Self {
            merge_gap_frac: p.merge_gap_frac,
            merge_area_ratio: p.merge_area_ratio,
            noise_min_area: p.noise_min_area,
            grid_regularity_min: p.grid_regularity_min,
        }
    }
}

impl From<SensitivityError> for CmdError {
    fn from(e: SensitivityError) -> Self {
        CmdError { message: e.message }
    }
}

/// One group in the overlay, canonical scan order.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupDto {
    /// `[x, y, w, h]` in sheet pixels — the box the overlay draws.
    pub bbox: [u32; 4],
    /// Ink area, in pixels.
    pub area: u32,
}

/// Why a group needs review (the §3.4 wording, one entry per warned group).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WarningDto {
    /// Index into [`GroupingDto::groups`].
    pub group: u32,
    /// `spansMultipleCells` | `restoredFromMerge` — the overlay's colour key.
    pub kind: &'static str,
    /// Short human label.
    pub label: &'static str,
}

/// The measured F5 lattice: the overlay draws these valleys as guides.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GridHintDto {
    /// Columns form a regular lattice.
    pub grid_x: bool,
    /// Rows form a regular lattice.
    pub grid_y: bool,
    /// Cells along x when `grid_x`.
    pub cells_x: u32,
    /// Cells along y when `grid_y`.
    pub cells_y: u32,
    /// Column valley centres, ascending.
    pub valley_x: Vec<u32>,
    /// Row valley centres, ascending.
    pub valley_y: Vec<u32>,
}

/// Refine evidence — what the automatic pass did to this sheet.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupStatsDto {
    /// Components handed to the refine chain.
    pub input: u32,
    /// Groups it produced.
    pub output: u32,
    /// Components merged away.
    pub merges: u32,
    /// Originals F5 restored from provably wrong merges.
    pub restored: u32,
    /// Groups flagged `SpansMultipleCells`.
    pub flagged: u32,
    /// Flagged components the watershed actually split.
    pub resplit: u32,
    /// Merge rounds that changed the grouping.
    pub iterations: u32,
    /// Median component height, in pixels.
    pub median_h: f32,
    /// Median component area, in pixels.
    pub median_area: f32,
    /// Whole-call wall-clock, in milliseconds.
    pub elapsed_ms: f32,
}

/// One grouping result — everything the overlay draws.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupingDto {
    /// Hex-encoded 16-byte sheet id.
    pub sheet_id: String,
    /// Sheet width in pixels (the coordinate space of every box below).
    pub width: u32,
    /// Sheet height in pixels.
    pub height: u32,
    /// Final groups, canonical scan order.
    pub groups: Vec<GroupDto>,
    /// Groups needing review, ascending.
    pub warnings: Vec<WarningDto>,
    /// `1 − Σ weightᵢ · signalᵢ`.
    pub confidence: f32,
    /// `warnings.len()`.
    pub review_groups: u32,
    /// The §3.4 status line.
    pub status_line: String,
    /// Wall-clock for this call, in milliseconds.
    pub elapsed_ms: f32,
    /// True when the mask came from the cache (no decode, no segmentation).
    pub mask_cache_hit: bool,
    /// Manual edits applied on top of the automatic result.
    pub manual_edits: u32,
    /// The slider values this result was grouped with.
    pub sensitivity: SensitivityDto,
    /// The measured F5 lattice.
    pub hint: GridHintDto,
    /// Refine evidence.
    pub stats: GroupStatsDto,
}

/// What Split Here did.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SplitHereDto {
    /// True when the component was actually cut.
    pub split: bool,
    /// Regions the watershed produced (0 when refused).
    pub regions: u32,
    /// Index of the group the pointer selected, before the edit.
    pub group_index: u32,
    /// Wall-clock for the edit (the ≤ 20 ms budget), in milliseconds.
    pub elapsed_ms: f32,
    /// The report after the edit (identical groups when the split was refused).
    pub report: GroupingDto,
}

/// The overlay's backdrop: a downscaled PNG of the normalized sheet.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SheetPreviewDto {
    /// Base64 PNG (no data-URL prefix).
    pub png: String,
    /// Preview width in pixels.
    pub width: u32,
    /// Preview height in pixels.
    pub height: u32,
    /// Normalized sheet width — the coordinate space groups live in.
    pub sheet_width: u32,
    /// Normalized sheet height.
    pub sheet_height: u32,
}

/// The session behind the live grouping UI, as commands see it.
type SessionGuard<'a> = std::sync::MutexGuard<'a, Option<GroupingSession>>;

/// 32-char hex form of a sheet id (the webview's `sheetId`).
fn hex16(id: &[u8; 16]) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}

/// The webview's colour key for a warning kind.
fn warning_kind(kind: WarningKind) -> &'static str {
    match kind {
        WarningKind::SpansMultipleCells => "spansMultipleCells",
        WarningKind::RestoredFromMerge => "restoredFromMerge",
    }
}

/// Serializes a report plus the parameters it was produced with.
fn grouping_dto(id: &[u8; 16], report: &GroupingReport, refine: &RefineParams) -> GroupingDto {
    GroupingDto {
        sheet_id: hex16(id),
        width: report.width,
        height: report.height,
        groups: report
            .groups
            .iter()
            .map(|g| GroupDto {
                bbox: [g.bbox.x, g.bbox.y, g.bbox.w, g.bbox.h],
                area: g.area,
            })
            .collect(),
        warnings: report
            .confidence
            .warnings
            .iter()
            .map(|w| WarningDto {
                group: w.group,
                kind: warning_kind(w.kind),
                label: w.kind.label(),
            })
            .collect(),
        confidence: report.confidence.score,
        review_groups: report.confidence.review_groups,
        status_line: report.status_line.clone(),
        elapsed_ms: report.elapsed_ms,
        mask_cache_hit: report.mask_cache_hit,
        manual_edits: report.manual_edits,
        sensitivity: SensitivityDto::from_native(SensitivityParams::from_refine(refine)),
        hint: GridHintDto {
            grid_x: report.hint.grid_x,
            grid_y: report.hint.grid_y,
            cells_x: report.hint.cells_x,
            cells_y: report.hint.cells_y,
            valley_x: report.hint.valley_x.clone(),
            valley_y: report.hint.valley_y.clone(),
        },
        stats: GroupStatsDto {
            input: report.stats.input,
            output: report.groups.len() as u32,
            merges: report.stats.merges,
            restored: report.stats.grid.restored,
            flagged: report.stats.grid.flagged,
            resplit: report.stats.grid.resplit,
            iterations: u32::from(report.stats.iterations),
            median_h: report.stats.median_h,
            median_area: report.stats.median_area,
            elapsed_ms: report.elapsed_ms,
        },
    }
}

/// The sheet's id and bytes; also fails fast when no project is open or the
/// sheet id is unknown, so a session is never created for a sheet that does not
/// exist.
fn sheet_bytes(state: &State<'_, App>, sheet_id: &str) -> CmdResult<([u8; 16], Vec<u8>)> {
    let id = parse_hex16(sheet_id).ok_or_else(|| CmdError {
        message: format!("bad sheet id: {sheet_id}"),
    })?;
    let guard = state.lock()?;
    let Some(lib) = guard.as_ref() else {
        return Err(CmdError {
            message: "no project is open".into(),
        });
    };
    let row = lib.sheet_by_id(&id)?.ok_or_else(|| CmdError {
        message: "sheet not found".into(),
    })?;
    let bytes = std::fs::read(&row.source_path).map_err(|e| CmdError {
        message: format!("read {}: {e}", row.source_path),
    })?;
    Ok((id, bytes))
}

/// The session slot, creating an empty one on first use (`None` until the UI
/// groups something, so no mask cache is allocated before it is needed).
fn session_slot<'a>(state: &'a State<'_, App>) -> CmdResult<SessionGuard<'a>> {
    state.grouping_slot()
}

/// Groups the sheet unless the session is already showing it, then hands the
/// session back for the caller's edit.
///
/// Grouping again from the same bytes is what "Group All" means: the automatic
/// result replaces whatever manual edits were on screen.
fn ensure_grouped<'a>(
    slot: &'a mut SessionGuard<'_>,
    bytes: &[u8],
) -> CmdResult<&'a mut GroupingSession> {
    let seg = SegParams::default();
    let key = mask_key(bytes, &seg);
    let session = slot.get_or_insert_with(|| GroupingSession::new(App::MASK_CACHE_SHEETS));
    if session.current_key() != Some(key.as_str()) {
        let (_, _, hit) = mask_cached(bytes, 4096, &seg, session.cache_mut())?;
        session.group_sheet(&key, hit).ok_or_else(|| CmdError {
            message: "grouping produced no report".into(),
        })?;
    }
    Ok(session)
}

/// Serializes whatever the session currently holds.
fn finish(session: &GroupingSession, id: &[u8; 16]) -> CmdResult<GroupingDto> {
    let report = session.current_report().ok_or_else(|| CmdError {
        message: "grouping produced no report".into(),
    })?;
    Ok(grouping_dto(id, &report, &session.params().refine))
}

/// Groups every icon on a sheet and returns the overlay payload.
///
/// The first call on a sheet decodes + segments it (and caches the mask); every
/// later call — including moving the sensitivity sliders — reuses that mask.
#[tauri::command]
pub fn group_all(state: State<'_, App>, sheet_id: String) -> CmdResult<GroupingDto> {
    let (id, bytes) = sheet_bytes(&state, &sheet_id)?;
    let mut slot = session_slot(&state)?;
    let session = ensure_grouped(&mut slot, &bytes)?;
    finish(session, &id)
}

/// Applies the sensitivity sliders and re-groups from the cached mask.
///
/// Rejected values leave the session untouched (the envelope is
/// [`SensitivityParams::RANGES`], which the sliders mirror).
#[tauri::command]
pub fn group_set_sensitivity(
    state: State<'_, App>,
    sheet_id: String,
    sensitivity: SensitivityDto,
) -> CmdResult<GroupingDto> {
    let (id, bytes) = sheet_bytes(&state, &sheet_id)?;
    let mut slot = session_slot(&state)?;
    let session = ensure_grouped(&mut slot, &bytes)?;
    let params = session.params_mut();
    sensitivity.native().apply(&mut params.refine)?;
    let refine = params.refine;
    session.set_refine(refine);
    finish(session, &id)
}

/// Split Here: cuts the group under `(x, y)` with the W9 watershed.
///
/// The click is the evidence the F2 size gate stood in for; every other guard
/// still applies, and a refusal leaves the group list untouched.
#[tauri::command]
pub fn group_split_here(
    state: State<'_, App>,
    sheet_id: String,
    x: u32,
    y: u32,
) -> CmdResult<SplitHereDto> {
    let (id, bytes) = sheet_bytes(&state, &sheet_id)?;
    let mut slot = session_slot(&state)?;
    let session = ensure_grouped(&mut slot, &bytes)?;
    let out = session.split_here(x, y).ok_or_else(|| CmdError {
        message: format!("no group at ({x}, {y})"),
    })?;
    let params = session.params().refine;
    Ok(SplitHereDto {
        split: out.split,
        regions: out.regions,
        group_index: out.group_index as u32,
        elapsed_ms: out.elapsed_ms,
        report: grouping_dto(&id, &out.report, &params),
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupSelectedRequest {
    /// Hex-encoded 16-byte sheet id.
    pub sheet_id: String,
    /// Marquee boxes in sheet pixels, `[x, y, w, h]` each.
    pub boxes: Vec<[u32; 4]>,
}

/// Group Selected: the marquee's groups collapse into one icon.
#[tauri::command]
pub fn group_selected(state: State<'_, App>, req: GroupSelectedRequest) -> CmdResult<GroupingDto> {
    if req.boxes.is_empty() {
        return Err(CmdError {
            message: "no marquee box".into(),
        });
    }
    let mut parsed = Vec::with_capacity(req.boxes.len());
    for [x, y, w, h] in req.boxes {
        parsed.push(Bbox::new(x, y, w, h).ok_or_else(|| CmdError {
            message: format!("empty marquee ({x}, {y}, {w}, {h})"),
        })?);
    }
    let (id, bytes) = sheet_bytes(&state, &req.sheet_id)?;
    let mut slot = session_slot(&state)?;
    let session = ensure_grouped(&mut slot, &bytes)?;
    session.group_selected(&parsed);
    finish(session, &id)
}

/// A downscaled PNG of the normalized sheet — the grouping overlay's backdrop.
///
/// A miss decodes + segments the sheet once and fills **both** the mask cache
/// (so the following `group_all` costs milliseconds) and the preview cache (so
/// resizing the overlay costs nothing).
#[tauri::command]
pub fn sheet_preview(
    state: State<'_, App>,
    sheet_id: String,
    max_dim: u32,
) -> CmdResult<SheetPreviewDto> {
    if max_dim == 0 || max_dim > PREVIEW_MAX_DIM {
        return Err(CmdError {
            message: format!("maxDim {max_dim} outside [1, {PREVIEW_MAX_DIM}]"),
        });
    }
    let (_, bytes) = sheet_bytes(&state, &sheet_id)?;
    let seg = SegParams::default();
    let key = mask_key(&bytes, &seg);
    let mut slot = session_slot(&state)?;
    let session = slot.get_or_insert_with(|| GroupingSession::new(App::MASK_CACHE_SHEETS));
    // Cloned up front so the miss path can borrow the session mutably.
    let cached = session.preview(&key, max_dim).cloned();
    let image = match cached {
        Some(image) => image,
        None => {
            let out = segment(&bytes, 4096, &seg)?;
            let (width, height, png) = out.sheet.preview_png(max_dim);
            let image = PreviewImage {
                sheet_width: out.sheet.width(),
                sheet_height: out.sheet.height(),
                width,
                height,
                png,
            };
            session.cache_mut().put(&key, &out.mask, &out.background);
            session.store_preview(&key, max_dim, image.clone());
            image
        }
    };
    use base64::Engine as _;
    Ok(SheetPreviewDto {
        png: base64::engine::general_purpose::STANDARD.encode(&image.png),
        width: image.width,
        height: image.height,
        sheet_width: image.sheet_width,
        sheet_height: image.sheet_height,
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
        sheet_icons,
        sheet_crop,
        group_all,
        group_set_sensitivity,
        group_split_here,
        group_selected,
        sheet_preview,
    ])
}
