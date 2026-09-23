//! IPC commands — the webview-facing surface. Thin validation +
//! [`isg_native`] forwarding; every long operation goes through the job
//! engine instead of blocking a command.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{Builder, State};

use isg_core::{Bbox, ForegroundMask, IconGroup, TracePreset};
use isg_native::cache::CacheStore;
use isg_native::db::{IconVectorRow, Library};
use isg_native::jobs::{JobEngine, JobId};
use isg_native::pipeline::{
    cached_vectorize, mask_cached, mask_key, normalize, segment, BackgroundModel, GroupingReport,
    GroupingSession, PreviewImage, RefineParams, SegParams, SensitivityError, SensitivityParams,
    VectorizeError, WarningKind,
};
use isg_native::review_host::{host_inputs, host_review};
use isg_native::review_native::{review_background, ReviewOptions};
use isg_native::sheet::export::{
    artwork_file, artwork_from_svg, derive_row, expand_pattern, grid_position, parse_csv, slugify,
    write_csv, write_sheet_pdf, write_sheet_svg, Artwork, Column, CsvOptions, IconMeta,
    PatternValues, PdfOptions, SheetRow, SvgOptions, CSV_COLUMNS,
};
use isg_native::sheet::metrics::IconMetrics;
use isg_native::sheet::{measure, IconInput, SheetPlan, SheetSpec};
use isg_native::sheet_native::{render_sheet_png, validate_sheet_svg, RasterOptions};

use crate::jobs::{cache_dir_for, ImportJob, VectorizeSheetJob};
use crate::review_cmds::{
    apply_decision, export, session_for, undo_decision, ReviewOut, ReviewSession, TriageActionDto,
    TriageStateOut,
};
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

impl From<isg_native::review_native::ReviewError> for CmdError {
    fn from(e: isg_native::review_native::ReviewError) -> Self {
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
#[serde(rename_all = "camelCase")]
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
    let row = lib.sheet_by_id(&id)?.ok_or_else(|| CmdError {
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

/// Lists the vectorized icons stored for one sheet (comparator grid).
#[tauri::command]
pub fn sheet_icons(state: State<'_, App>, sheet_id: String) -> CmdResult<Vec<IconDto>> {
    let id = parse_hex16(&sheet_id).ok_or_else(|| CmdError {
        message: format!("bad sheet id: {sheet_id}"),
    })?;
    let guard = state.lock()?;
    let Some(lib) = guard.as_ref() else {
        return Err(CmdError {
            message: "no project is open".into(),
        });
    };
    let rows = lib.icons_for_sheet(&id)?;
    Ok(rows.iter().map(IconDto::from).collect())
}

/// Base64 PNG of one icon's crop from the normalized sheet — the exact
/// pixels stage ⑧ scored (comparator A-side).
#[tauri::command]
pub fn sheet_crop(
    state: State<'_, App>,
    sheet_id: String,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
) -> CmdResult<String> {
    let id = parse_hex16(&sheet_id).ok_or_else(|| CmdError {
        message: format!("bad sheet id: {sheet_id}"),
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
    let row = lib.sheet_by_id(&id)?.ok_or_else(|| CmdError {
        message: "sheet not found".into(),
    })?;
    let bytes = std::fs::read(&row.source_path).map_err(|e| CmdError {
        message: format!("read {}: {e}", row.source_path),
    })?;
    let out = segment(&bytes, 4096, &SegParams::default())?;
    let png = out.sheet.crop_png(bbox);
    use base64::Engine as _;
    Ok(base64::engine::general_purpose::STANDARD.encode(png))
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
    let image = match session.preview(&key, max_dim) {
        Some(cached) => cached.clone(),
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

// ---------------------------------------------------------------------------
// Sheet generator (§3.5) — the plan, the derived metadata and the exports
// ---------------------------------------------------------------------------

/// The cell spec the wizard sends. Every field has a default, so a partial
/// request is a valid request — and every value is clamped by
/// [`SheetSpec::from_wire`] rather than rejected, because these are sliders.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SheetSpecDto {
    /// Cell side in pixels.
    pub cell: u32,
    /// Padding inside each cell.
    pub padding: u32,
    /// Gap between cells.
    pub gap: u32,
    /// Margin around the sheet.
    pub margin: u32,
    /// Cells per row (a maximum: the sheet shrinks to its content).
    pub columns: u32,
    /// Fraction of the inner box the ink should fill.
    pub ink_ratio: f32,
    /// `center` | `optical` | `baseline`.
    pub placement: String,
}

impl Default for SheetSpecDto {
    fn default() -> Self {
        let spec = SheetSpec::default();
        Self {
            cell: spec.cell,
            padding: spec.padding,
            gap: spec.gap,
            margin: spec.margin,
            columns: spec.columns,
            ink_ratio: spec.ink_ratio,
            placement: spec.placement.as_str().to_string(),
        }
    }
}

impl SheetSpecDto {
    /// The clamped, machine-side spec.
    #[must_use]
    pub fn to_spec(&self) -> SheetSpec {
        SheetSpec::from_wire(
            self.cell,
            self.padding,
            self.gap,
            self.margin,
            self.columns,
            self.ink_ratio,
            &self.placement,
        )
    }
}

/// The CSV wizard's formatting choices. The *names* are derived once, at plan
/// time, so the file, the CSV and the SVG's layer ids cannot disagree.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SheetCsvDto {
    /// One character: `,` or `;` or a tab.
    pub delimiter: String,
    /// Write a header row.
    pub header: bool,
    /// Column names, in order. Unknown names are dropped; an empty list means
    /// the default columns.
    pub columns: Vec<String>,
}

impl Default for SheetCsvDto {
    fn default() -> Self {
        Self {
            delimiter: ",".to_string(),
            header: true,
            columns: CSV_COLUMNS.iter().map(|c| c.header().to_string()).collect(),
        }
    }
}

impl SheetCsvDto {
    /// The writer's options (an unknown delimiter falls back to a comma).
    #[must_use]
    pub fn to_options(&self, name_pattern: &str) -> CsvOptions {
        CsvOptions {
            delimiter: self.delimiter.as_bytes().first().copied().unwrap_or(b','),
            header: self.header,
            name_pattern: name_pattern.to_string(),
        }
    }

    /// The columns to write, in the order the wizard listed them.
    #[must_use]
    pub fn to_columns(&self) -> Vec<Column> {
        let mut columns: Vec<Column> = self
            .columns
            .iter()
            .filter_map(|name| Column::parse(name))
            .collect();
        if columns.is_empty() {
            columns = CSV_COLUMNS.to_vec();
        }
        columns
    }
}

/// A plan request: which sheet, how to lay it out, and what to call things.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SheetPlanRequest {
    /// 32-char hex sheet id.
    pub sheet_id: String,
    /// Cell spec (every field defaulted).
    #[serde(default)]
    pub spec: SheetSpecDto,
    /// Names the `{sheet}` token; defaults to the source file's stem.
    #[serde(default)]
    pub sheet_stem: Option<String>,
    /// The preset label that goes into the metadata and the tags.
    #[serde(default = "default_preset_name")]
    pub preset: String,
    /// Name pattern: `{sheet}`, `{index}`, `{row}`, `{col}`, `{preset}`, with
    /// optional zero-padding (`{index:03}`).
    #[serde(default = "default_name_pattern")]
    pub name_pattern: String,
}

fn default_preset_name() -> String {
    "flat-8".to_string()
}

fn default_name_pattern() -> String {
    CsvOptions::default().name_pattern
}

/// An export request: a plan request plus what to write and where.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SheetExportRequest {
    /// Everything a plan needs.
    #[serde(flatten)]
    pub plan: SheetPlanRequest,
    /// The CSV wizard's choices.
    #[serde(default)]
    pub csv: SheetCsvDto,
    /// `svg`, `pdf`, `png`, `csv` and/or `icons` (one SVG per icon, the layout
    /// the CSV's `file` column points at). Empty means all of the first four.
    #[serde(default)]
    pub formats: Vec<String>,
    /// Directory the files are written into (created if missing).
    pub out_dir: String,
    /// Pixels per sheet pixel for the PNG.
    #[serde(default = "default_raster_scale")]
    pub raster_scale: f32,
    /// `#rrggbb` paper colour; omitted keeps the sheet transparent.
    #[serde(default)]
    pub background: Option<String>,
}

fn default_raster_scale() -> f32 {
    1.0
}

/// The leveling report as the wizard's summary panel shows it.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SheetReportDto {
    /// Coefficient of variation of the placed ink size (the headline number).
    pub ink_size_cv: f32,
    /// CV of the icons' stroke weights as measured.
    pub stroke_cv: f32,
    /// Worst row's baseline spread, in sheet pixels.
    pub baseline_spread: f32,
    /// The stroke weight the sheet was leveled against.
    pub median_stroke: f32,
    /// The solidity the sheet was leveled against.
    pub median_solidity: f32,
    /// Icons that backed off to a pure fit.
    pub overflow_backoffs: u32,
    /// Icons whose stroke correction hit a clamp.
    pub stroke_clamped: u32,
    /// Icons whose solidity correction hit a clamp.
    pub solidity_clamped: u32,
}

impl From<&isg_native::sheet::LevelReport> for SheetReportDto {
    fn from(r: &isg_native::sheet::LevelReport) -> Self {
        Self {
            ink_size_cv: r.ink_size_cv,
            stroke_cv: r.stroke_cv,
            baseline_spread: r.baseline_spread,
            median_stroke: r.median_stroke,
            median_solidity: r.median_solidity,
            overflow_backoffs: r.overflow_backoffs as u32,
            stroke_clamped: r.stroke_clamped as u32,
            solidity_clamped: r.solidity_clamped as u32,
        }
    }
}

/// One icon's place on the sheet, plus the metadata derived for it — the same
/// numbers the preview draws and the CSV row carries.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SheetPlacementDto {
    /// Icon id (1-based reading-order position).
    pub id: u32,
    /// 1-based reading-order position.
    pub index: u32,
    /// 1-based grid row.
    pub row: u32,
    /// 1-based grid column.
    pub col: u32,
    /// Cell's top-left corner in sheet pixels.
    pub cell_x: u32,
    /// Cell's top-left corner in sheet pixels.
    pub cell_y: u32,
    /// Placed ink box, in sheet pixels.
    pub x: f32,
    /// Placed ink box, in sheet pixels.
    pub y: f32,
    /// Placed ink box width.
    pub w: f32,
    /// Placed ink box height.
    pub h: f32,
    /// Crop pixels → sheet pixels.
    pub scale: f32,
    /// `overflow`, `strokeClamped`, `solidityClamped`.
    pub flags: Vec<String>,
    /// Derived name (the pattern expanded).
    pub name: String,
    /// Derived slug.
    pub slug: String,
    /// Where this icon's own SVG goes inside an export.
    pub file: String,
}

/// The whole plan: the sheet's size, the report, and one entry per icon.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SheetPlanDto {
    /// Sheet width in pixels.
    pub width: u32,
    /// Sheet height in pixels.
    pub height: u32,
    /// Columns actually used.
    pub columns: u32,
    /// Rows actually used.
    pub rows: u32,
    /// Icons placed.
    pub icons: u32,
    /// Cell side in pixels (after clamping).
    pub cell: u32,
    /// Ink ratio in force.
    pub ink_ratio: f32,
    /// Placement mode in force.
    pub placement: String,
    /// The leveling report.
    pub report: SheetReportDto,
    /// One entry per icon, in reading order.
    pub placements: Vec<SheetPlacementDto>,
}

/// One written file.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SheetFileDto {
    /// `svg` | `pdf` | `png` | `csv` | `icons`.
    pub format: String,
    /// Absolute path (a directory for `icons`).
    pub path: String,
    /// Bytes written (0 for a directory).
    pub bytes: u64,
    /// What was checked before the file was kept — the second reader each
    /// format passed, with its numbers.
    pub evidence: String,
}

/// An export's result: the plan it was made from, and every file.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SheetExportDto {
    /// The plan the files were written from.
    pub plan: SheetPlanDto,
    /// The files, in the order they were written.
    pub files: Vec<SheetFileDto>,
}

/// The CSV wizard's preview: the table, and the file itself.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SheetCsvPreviewDto {
    /// Column headers, in order.
    pub columns: Vec<String>,
    /// One row of formatted values per icon.
    pub rows: Vec<Vec<String>>,
    /// The file as it would be written (the wizard's preview pane).
    pub text: String,
}

/// Everything one build produces; shared by the three sheet commands.
struct SheetBuild {
    plan: SheetPlan,
    metrics: Vec<IconMetrics>,
    /// `(name, slug, file)` per placement, in the plan's order.
    names: Vec<(String, String, String)>,
    artwork: Vec<Artwork>,
    /// The `{sheet}` token's value, as the plan derived it.
    stem: String,
    /// The preset label the metadata and the tags carry.
    preset: String,
    /// The name pattern the names above were expanded from.
    name_pattern: String,
}

/// Groups the sheet through the session (reusing a warm mask), then hands back
/// the groups, the mask they were cut from and the background model.
fn sheet_groups(
    state: &State<'_, App>,
    bytes: &[u8],
) -> CmdResult<(Vec<IconGroup>, ForegroundMask, BackgroundModel)> {
    let groups: Vec<IconGroup> = {
        let mut slot = session_slot(state)?;
        let session = ensure_grouped(&mut slot, bytes)?;
        session
            .current_report()
            .map(|report| report.groups.clone())
            .unwrap_or_default()
    };
    if groups.is_empty() {
        return Err(CmdError {
            message: "the sheet has no groups to place".into(),
        });
    }
    let seg = SegParams::default();
    let (mask, background, _hit) = {
        let mut slot = session_slot(state)?;
        let session = slot.get_or_insert_with(|| GroupingSession::new(App::MASK_CACHE_SHEETS));
        mask_cached(bytes, 4096, &seg, session.cache_mut())?
    };
    Ok((groups, mask, background))
}

/// The `{sheet}` token's value: the request's override, else the file stem.
fn sheet_stem(state: &State<'_, App>, req: &SheetPlanRequest) -> String {
    if let Some(stem) = req.sheet_stem.as_ref() {
        if !stem.trim().is_empty() {
            return stem.clone();
        }
    }
    let Some(id) = parse_hex16(&req.sheet_id) else {
        return "sheet".to_string();
    };
    let Ok(guard) = state.lock() else {
        return "sheet".to_string();
    };
    let Some(lib) = guard.as_ref() else {
        return "sheet".to_string();
    };
    let Ok(Some(row)) = lib.sheet_by_id(&id) else {
        return "sheet".to_string();
    };
    PathBuf::from(&row.source_path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "sheet".to_string())
}

/// Builds the plan, the derived names and (when asked) the artwork.
///
/// The artwork is each icon's *shipping* SVG — the cached, usvg-validated
/// document stage ⑦ produced — so a sheet never contains geometry that has not
/// already passed the emitter's gate.
fn sheet_build(
    state: &State<'_, App>,
    req: &SheetPlanRequest,
    want_artwork: bool,
) -> CmdResult<SheetBuild> {
    let (_id, bytes) = sheet_bytes(state, &req.sheet_id)?;
    if preset_from_name(&req.preset).is_none() {
        return Err(CmdError {
            message: format!("unknown preset: {}", req.preset),
        });
    }
    let (groups, mask, background) = sheet_groups(state, &bytes)?;
    let spec = req.spec.to_spec();
    let stem = sheet_stem(state, req);

    let mut icons = Vec::with_capacity(groups.len());
    let mut metrics = Vec::with_capacity(groups.len());
    for (index, group) in groups.iter().enumerate() {
        let measured = measure(&mask, group.bbox).ok_or_else(|| CmdError {
            message: format!("group {index} at {:?} has no ink", group.bbox),
        })?;
        metrics.push(measured);
        icons.push(IconInput {
            id: index as u32 + 1,
            metrics: measured,
        });
    }
    let plan = SheetPlan::new(&icons, spec);

    let mut names = Vec::with_capacity(plan.placements.len());
    for placement in &plan.placements {
        let (row, col) = grid_position(placement, plan.layout.columns);
        let name = expand_pattern(
            &req.name_pattern,
            &PatternValues {
                sheet: &stem,
                index: placement.id,
                row,
                col,
                preset: &req.preset,
            },
        );
        let slug = slugify(&name);
        names.push((name, slug.clone(), artwork_file(&slug)));
    }

    let artwork = if want_artwork {
        trace_artwork(state, &bytes, &groups, &background, &names)?
    } else {
        Vec::new()
    };
    Ok(SheetBuild {
        plan,
        metrics,
        names,
        artwork,
        stem,
        preset: req.preset.clone(),
        name_pattern: req.name_pattern.clone(),
    })
}

/// Traces (or reads from the cache) every group's document and parses it into
/// placeable artwork.
///
/// This holds the project lock while it runs, exactly like `vectorize_icon`
/// does for one icon: a 1000-icon export is a batch, and a job-engine-backed
/// export with progress reporting is a Phase-7 hardening item.
fn trace_artwork(
    state: &State<'_, App>,
    bytes: &[u8],
    groups: &[IconGroup],
    background: &BackgroundModel,
    names: &[(String, String, String)],
) -> CmdResult<Vec<Artwork>> {
    let guard = state.lock()?;
    let Some(lib) = guard.as_ref() else {
        return Err(CmdError {
            message: "no project is open".into(),
        });
    };
    let sheet = normalize(bytes, 4096)?;
    let store = CacheStore::new(cache_dir_for(lib.path()));
    let seg_params = SegParams::default();
    let mut artwork = Vec::with_capacity(groups.len());
    for (index, group) in groups.iter().enumerate() {
        let (icon, _cached) = cached_vectorize(
            &store,
            lib,
            &sheet,
            group.bbox,
            background,
            TracePreset::Balanced,
            &seg_params.to_cache_string(),
        )?;
        let name = names
            .get(index)
            .map(|n| n.0.clone())
            .unwrap_or_else(|| format!("icon-{:03}", index + 1));
        artwork.push(
            artwork_from_svg(index as u32 + 1, name, &icon.svg).map_err(|e| CmdError {
                message: e.to_string(),
            })?,
        );
    }
    Ok(artwork)
}

/// Turns a build into the plan DTO the wizard renders.
fn plan_dto(build: &SheetBuild) -> SheetPlanDto {
    let placements: Vec<SheetPlacementDto> = build
        .plan
        .placements
        .iter()
        .enumerate()
        .map(|(index, placement)| {
            let (row, col) = grid_position(placement, build.plan.layout.columns);
            let (name, slug, file) = build.names[index].clone();
            let mut flags = Vec::new();
            if placement.flags & isg_native::sheet::FLAG_OVERFLOW_BACKOFF != 0 {
                flags.push("overflow".to_string());
            }
            if placement.flags & isg_native::sheet::FLAG_STROKE_CLAMPED != 0 {
                flags.push("strokeClamped".to_string());
            }
            if placement.flags & isg_native::sheet::FLAG_SOLIDITY_CLAMPED != 0 {
                flags.push("solidityClamped".to_string());
            }
            SheetPlacementDto {
                id: placement.id,
                index: index as u32 + 1,
                row,
                col,
                cell_x: placement.cell.0,
                cell_y: placement.cell.1,
                x: placement.ink.x,
                y: placement.ink.y,
                w: placement.ink.w,
                h: placement.ink.h,
                scale: placement.scale,
                flags,
                name,
                slug,
                file,
            }
        })
        .collect();
    SheetPlanDto {
        width: build.plan.layout.width,
        height: build.plan.layout.height,
        columns: build.plan.layout.columns,
        rows: build.plan.layout.rows,
        icons: placements.len() as u32,
        cell: build.plan.spec.cell,
        ink_ratio: build.plan.spec.ink_ratio,
        placement: build.plan.spec.placement.as_str().to_string(),
        report: SheetReportDto::from(&build.plan.report),
        placements,
    }
}

/// Plans a sheet: layout, §3.5 leveling, and the metadata derived for every
/// icon. Cheap enough to call on every slider move — it is arithmetic over the
/// cached mask, and §3.5 budgets 1000 icons in 20 ms.
#[tauri::command]
pub fn sheet_plan(state: State<'_, App>, req: SheetPlanRequest) -> CmdResult<SheetPlanDto> {
    let build = sheet_build(&state, &req, false)?;
    Ok(plan_dto(&build))
}

/// Previews the CSV the wizard would write, without writing anything. The file
/// is re-read by its own parser before it is shown, so a preview of a broken
/// file cannot be presented as a preview of a good one.
#[tauri::command]
pub fn sheet_csv_preview(
    state: State<'_, App>,
    req: SheetPlanRequest,
    csv: SheetCsvDto,
) -> CmdResult<SheetCsvPreviewDto> {
    let build = sheet_build(&state, &req, false)?;
    let columns = csv.to_columns();
    let rows = csv_rows(&build);
    let options = csv.to_options(&req.name_pattern);
    let text = write_csv(&rows, &columns, &options).map_err(|e| CmdError {
        message: e.to_string(),
    })?;
    let parsed = parse_csv(&text, options.delimiter).map_err(|e| CmdError {
        message: e.to_string(),
    })?;
    Ok(SheetCsvPreviewDto {
        columns: columns.iter().map(|c| c.header().to_string()).collect(),
        rows: parsed
            .into_iter()
            .skip(usize::from(options.header))
            .collect(),
        text,
    })
}

/// Derives the CSV's rows from a build.
///
/// The row is `derive_row`'s, not a second derivation: the wizard's preview, the
/// written manifest and the plan all expand the same pattern from the same
/// stem, preset and metrics. The file name comes from the build because that is
/// what the export actually writes.
fn csv_rows(build: &SheetBuild) -> Vec<SheetRow> {
    let options = CsvOptions {
        name_pattern: build.name_pattern.clone(),
        ..CsvOptions::default()
    };
    let mut rows = Vec::with_capacity(build.plan.placements.len());
    for (index, placement) in build.plan.placements.iter().enumerate() {
        let meta = IconMeta {
            id: placement.id,
            id_hex: format!("{:08x}", placement.id),
            sheet_stem: build.stem.clone(),
            preset: build.preset.clone(),
            // The artwork knows how many filled regions the icon has; without
            // it (a plan-only call) the column is unknown rather than invented.
            colours: build
                .artwork
                .iter()
                .find(|a| a.id == placement.id)
                .map_or(0, |a| a.shapes.len() as u32),
        };
        rows.push(derive_row(
            &meta,
            placement,
            &build.metrics[index],
            build.plan.layout.columns,
            &build.names[index].2,
            &options,
        ));
    }
    rows
}

/// Exports the sheet: SVG, PDF, PNG, CSV, and optionally one SVG per icon.
///
/// Every file is checked by a *second* reader before it is reported: the sheet
/// SVG by `usvg` (the engine an outside viewer behaves like), the PDF by this
/// crate's own strict reader, the PNG by its reader after `resvg` rendered it,
/// each per-icon SVG by `usvg`, and the CSV by its own parser. The checks'
/// numbers come back as `evidence`, so the UI can show why an export is
/// trustworthy instead of asking for trust.
#[tauri::command]
pub fn sheet_export(state: State<'_, App>, req: SheetExportRequest) -> CmdResult<SheetExportDto> {
    let formats: Vec<String> = if req.formats.is_empty() {
        ["svg", "pdf", "png", "csv"]
            .iter()
            .map(|f| (*f).to_string())
            .collect()
    } else {
        req.formats
            .iter()
            .map(|f| f.trim().to_ascii_lowercase())
            .collect()
    };
    let wants_icons = formats.iter().any(|f| f.as_str() == "icons");
    let build = sheet_build(&state, &req.plan, true)?;
    let plan = &build.plan;
    let artwork = &build.artwork;
    let out_dir = PathBuf::from(&req.out_dir);
    std::fs::create_dir_all(&out_dir).map_err(|e| CmdError {
        message: format!("create {}: {e}", out_dir.display()),
    })?;
    let stem = sheet_stem(&state, &req.plan);
    let background = req.background.as_deref().and_then(parse_hex_color);
    let title = format!("{stem} sheet");

    let mut files = Vec::new();
    if formats.iter().any(|f| f.as_str() == "svg") {
        let svg = write_sheet_svg(
            plan,
            artwork,
            &SvgOptions {
                title: title.clone(),
                background,
                name_layers: true,
            },
        )
        .map_err(|e| CmdError {
            message: e.to_string(),
        })?;
        let (w, h) = validate_sheet_svg(&svg).map_err(|e| CmdError { message: e })?;
        let parsed = isg_core::editor::svg::parse(&svg).map_err(|e| CmdError {
            message: e.message().to_string(),
        })?;
        let path = out_dir.join(format!("{stem}-sheet.svg"));
        std::fs::write(&path, svg.as_bytes()).map_err(|e| CmdError {
            message: format!("write {}: {e}", path.display()),
        })?;
        files.push(SheetFileDto {
            format: "svg".into(),
            path: path.to_string_lossy().to_string(),
            bytes: svg.len() as u64,
            evidence: format!(
                "usvg read {w:.0}×{h:.0}; engine re-parsed {} paths",
                parsed.shapes.len()
            ),
        });
    }
    if formats.iter().any(|f| f.as_str() == "pdf") {
        let pdf = write_sheet_pdf(
            plan,
            artwork,
            &PdfOptions {
                background,
                title: title.clone(),
                ..PdfOptions::default()
            },
        )
        .map_err(|e| CmdError {
            message: e.to_string(),
        })?;
        let summary =
            isg_native::sheet::export::validate_pdf(&pdf).map_err(|e| CmdError { message: e })?;
        let path = out_dir.join(format!("{stem}-sheet.pdf"));
        std::fs::write(&path, &pdf).map_err(|e| CmdError {
            message: format!("write {}: {e}", path.display()),
        })?;
        files.push(SheetFileDto {
            format: "pdf".into(),
            path: path.to_string_lossy().to_string(),
            bytes: pdf.len() as u64,
            evidence: format!(
                "{} page(s), {:.0}×{:.0} pt, {} fills, all verified by the PDF reader",
                summary.pages, summary.media_box.0, summary.media_box.1, summary.fills
            ),
        });
    }
    if formats.iter().any(|f| f.as_str() == "png") {
        let raster = render_sheet_png(
            plan,
            artwork,
            &RasterOptions {
                svg: SvgOptions {
                    title: title.clone(),
                    background,
                    name_layers: true,
                },
                scale: req.raster_scale,
                background,
            },
        )
        .map_err(|e| CmdError {
            message: e.to_string(),
        })?;
        let info = isg_native::sheet::export::parse_png(&raster.png).map_err(|e| CmdError {
            message: e.to_string(),
        })?;
        let path = out_dir.join(format!("{stem}-sheet.png"));
        std::fs::write(&path, &raster.png).map_err(|e| CmdError {
            message: format!("write {}: {e}", path.display()),
        })?;
        files.push(SheetFileDto {
            format: "png".into(),
            path: path.to_string_lossy().to_string(),
            bytes: raster.png.len() as u64,
            evidence: format!(
                "resvg rendered {}×{} px at {:.1}x; the PNG reader re-read {}×{} px, {} B of pixels",
                raster.width, raster.height, raster.scale, info.width, info.height, info.idat_bytes
            ),
        });
    }
    if wants_icons {
        let dir = out_dir.join("svg");
        std::fs::create_dir_all(&dir).map_err(|e| CmdError {
            message: format!("create {}: {e}", dir.display()),
        })?;
        let mut written = 0usize;
        for (index, art) in artwork.iter().enumerate() {
            let one = single_icon_plan(plan, index);
            let svg = write_sheet_svg(
                &one,
                std::slice::from_ref(art),
                &SvgOptions {
                    title: art.name.clone(),
                    background: None,
                    name_layers: false,
                },
            )
            .map_err(|e| CmdError {
                message: e.to_string(),
            })?;
            validate_sheet_svg(&svg).map_err(|e| CmdError { message: e })?;
            let (_, slug, _) = build.names[index].clone();
            let path = dir.join(format!("{slug}.svg"));
            std::fs::write(&path, svg.as_bytes()).map_err(|e| CmdError {
                message: format!("write {}: {e}", path.display()),
            })?;
            written += 1;
        }
        files.push(SheetFileDto {
            format: "icons".into(),
            path: dir.to_string_lossy().to_string(),
            bytes: 0,
            evidence: format!("{written} icon documents, each re-read by usvg"),
        });
    }
    if formats.iter().any(|f| f.as_str() == "csv") {
        let columns = req.csv.to_columns();
        let options = req.csv.to_options(&req.plan.name_pattern);
        let rows = csv_rows(&build);
        let text = write_csv(&rows, &columns, &options).map_err(|e| CmdError {
            message: e.to_string(),
        })?;
        let parsed = parse_csv(&text, options.delimiter).map_err(|e| CmdError {
            message: e.to_string(),
        })?;
        let path = out_dir.join(format!("{stem}-icons.csv"));
        std::fs::write(&path, text.as_bytes()).map_err(|e| CmdError {
            message: format!("write {}: {e}", path.display()),
        })?;
        files.push(SheetFileDto {
            format: "csv".into(),
            path: path.to_string_lossy().to_string(),
            bytes: text.len() as u64,
            evidence: format!(
                "{} columns × {} rows, re-read by the CSV parser",
                columns.len(),
                parsed.len().saturating_sub(usize::from(options.header))
            ),
        });
    }

    Ok(SheetExportDto {
        plan: plan_dto(&build),
        files,
    })
}

/// A one-icon plan for writing an icon's own document: the same cell and ink
/// box it has on the sheet, but alone on a page of its own.
fn single_icon_plan(plan: &SheetPlan, index: usize) -> SheetPlan {
    let spec = SheetSpec {
        columns: 1,
        ..plan.spec
    };
    let icons: Vec<IconInput> = match plan.placements.get(index) {
        Some(p) => vec![IconInput {
            id: p.id,
            metrics: IconMetrics {
                ink_x: p.ink_local.0,
                ink_y: p.ink_local.1,
                ink_w: p.ink_local.2,
                ink_h: p.ink_local.3,
                ink_area: 0,
                centroid_x: p.ink_local.0 + p.ink_local.2 * 0.5,
                centroid_y: p.ink_local.1 + p.ink_local.3 * 0.5,
                stroke: 0.0,
                solidity: 1.0,
            },
        }],
        None => Vec::new(),
    };
    SheetPlan::new(&icons, spec)
}

/// Parses `#rrggbb` (or `#rgb`) into RGBA at full alpha.
fn parse_hex_color(text: &str) -> Option<[u8; 4]> {
    let hex = text.trim().trim_start_matches('#');
    // Byte-slicing below is only safe on ASCII, and a colour is ASCII.
    if !hex.is_ascii() {
        return None;
    }
    let expand = |c: char| -> Option<u8> {
        let d = c.to_digit(16)?;
        Some(((d * 16 + d) & 0xff) as u8)
    };
    let bytes: Vec<u8> = match hex.len() {
        3 => hex.chars().map(expand).collect::<Option<Vec<u8>>>()?,
        6 => (0..3)
            .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok())
            .collect::<Option<Vec<u8>>>()?,
        _ => return None,
    };
    Some([bytes[0], bytes[1], bytes[2], 255])
}

/// The review session over one sheet's rows, rebuilt from the triage journal.
///
/// The journal — not a cache held in this process — is the session's memory, so
/// a sheet reviewed in an earlier run of the app reloads with its decisions and
/// its undo stack intact (see [`crate::review_cmds`]).
fn review_session(lib: &Library, sheet: [u8; 16]) -> CmdResult<ReviewSession> {
    let rows = lib.icons_for_sheet(&sheet)?;
    let icons = rows.iter().map(|row| row.id).collect::<Vec<[u8; 16]>>();
    Ok(session_for(sheet, icons, lib)?)
}

/// What an undo changed.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UndoDto {
    /// The triage state after the undo.
    pub triage: TriageStateOut,
    /// 32-char hex of the icon whose decision was undone.
    pub icon: String,
}

/// The review pass's export request.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewExportRequest {
    /// The sheet to export.
    pub sheet_id: String,
    /// Directory to write `review-<sheet>.csv` into. Omitted (or empty) returns
    /// the CSV without touching the disk, which is what a preview needs.
    #[serde(default)]
    pub out_dir: Option<String>,
}

/// `review.csv` and where it landed.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewExportDto {
    /// The CSV text, exactly as written.
    pub csv: String,
    /// How many decisions it holds.
    pub decisions: u32,
    /// The next sequence number the session will hand out — a reader comparing
    /// two exports of one session can tell nothing was lost from the gap.
    pub seq: u64,
    /// The file, when one was written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// Runs the review pass for one sheet: quality, duplicates and outliers.
///
/// `async` because the pass is seconds of work on a real sheet (≈17 s at 1000
/// icons) and a synchronous command would hold the webview's main thread for
/// all of it. It reports no progress — the pass is one bounded operation whose
/// stages the report itself accounts for (`renderMs`, `detectMs`, the cascade
/// funnel) — so it is a command rather than a job; a sheet long enough to want
/// a progress bar would want a job instead.
#[tauri::command]
pub async fn review_run(state: State<'_, App>, sheet_id: String) -> CmdResult<ReviewOut> {
    let (id, bytes) = sheet_bytes(&state, &sheet_id)?;
    let sheet = normalize(&bytes, 4096)?;
    // The mask the overlay groups from is the mask the review measures with, and
    // the background it detects is the colour the document is composited over:
    // one segmentation, so the boxes in the report are the boxes on screen and
    // the ink plane is read the way CI reads it.
    let (mask, background) = {
        let mut slot = session_slot(&state)?;
        let session = slot.get_or_insert_with(|| GroupingSession::new(App::MASK_CACHE_SHEETS));
        let seg = SegParams::default();
        let (mask, background, _hit) = mask_cached(&bytes, 4096, &seg, session.cache_mut())?;
        (mask, review_background(&background))
    };

    let mut guard = state.lock()?;
    let Some(lib) = guard.as_mut() else {
        return Err(CmdError {
            message: "no project is open".into(),
        });
    };
    let store = CacheStore::new(cache_dir_for(lib.path()));
    let rows = lib.icons_for_sheet(&id)?;
    // Documents come from the stage ⑧ payloads the vectorizer already wrote, so
    // reviewing a traced sheet costs no extra tracing.
    let icons = host_inputs(&rows, &store, lib)?;
    let options = ReviewOptions {
        background,
        ..ReviewOptions::default()
    };
    let report = host_review(&sheet, &mask, &icons, &options)?;
    let session = review_session(lib, id)?;
    Ok(crate::review_cmds::review_out(&report, &session))
}

/// Records one triage decision, by the icon's id.
///
/// The icon id is the webview's handle; the sheet row it names is what the log
/// and the export use, and the mapping is never made in the frontend.
#[tauri::command]
pub fn review_apply(
    state: State<'_, App>,
    sheet_id: String,
    icon_id: String,
    action: String,
) -> CmdResult<TriageStateOut> {
    let id = parse_hex16(&sheet_id).ok_or_else(|| CmdError {
        message: format!("bad sheet id: {sheet_id}"),
    })?;
    let icon = parse_hex16(&icon_id).ok_or_else(|| CmdError {
        message: format!("bad icon id: {icon_id}"),
    })?;
    let action = TriageActionDto::parse(&action).ok_or_else(|| CmdError {
        message: format!("unknown triage action: {action}"),
    })?;
    let mut guard = state.lock()?;
    let Some(lib) = guard.as_mut() else {
        return Err(CmdError {
            message: "no project is open".into(),
        });
    };
    let mut session = review_session(lib, id)?;
    Ok(apply_decision(&mut session, lib, icon, action)?)
}

/// Takes back the most recent decision of one sheet's session.
///
/// `None` when there is nothing left to undo — a keystroke the workspace should
/// not have offered, so it is reported rather than turned into an error.
#[tauri::command]
pub fn review_undo(state: State<'_, App>, sheet_id: String) -> CmdResult<Option<UndoDto>> {
    let id = parse_hex16(&sheet_id).ok_or_else(|| CmdError {
        message: format!("bad sheet id: {sheet_id}"),
    })?;
    let mut guard = state.lock()?;
    let Some(lib) = guard.as_mut() else {
        return Err(CmdError {
            message: "no project is open".into(),
        });
    };
    let mut session = review_session(lib, id)?;
    Ok(
        undo_decision(&mut session, lib)?.map(|(triage, icon)| UndoDto {
            triage,
            icon: crate::review_cmds::hex32(icon),
        }),
    )
}

/// Renders `review.csv` (and writes it, when `outDir` is given).
#[tauri::command]
pub fn review_export(
    state: State<'_, App>,
    req: ReviewExportRequest,
) -> CmdResult<ReviewExportDto> {
    let id = parse_hex16(&req.sheet_id).ok_or_else(|| CmdError {
        message: format!("bad sheet id: {}", req.sheet_id),
    })?;
    let guard = state.lock()?;
    let Some(lib) = guard.as_ref() else {
        return Err(CmdError {
            message: "no project is open".into(),
        });
    };
    let session = review_session(lib, id)?;
    let out = export(&session);
    let path = match req.out_dir.as_deref().map(str::trim) {
        Some("") | None => None,
        Some(dir) => {
            let dir = PathBuf::from(dir);
            std::fs::create_dir_all(&dir).map_err(|e| CmdError {
                message: format!("create {}: {e}", dir.display()),
            })?;
            let file = dir.join(format!("review-{}.csv", crate::review_cmds::hex32(id)));
            std::fs::write(&file, out.csv.as_bytes()).map_err(|e| CmdError {
                message: format!("write {}: {e}", file.display()),
            })?;
            Some(file.to_string_lossy().into_owned())
        }
    };
    Ok(ReviewExportDto {
        csv: out.csv,
        decisions: out.decisions,
        seq: out.seq,
        path,
    })
}

/// The triage state of one sheet: what has been decided, counting what an undo
/// has taken back, and the size of the export it would produce.
#[tauri::command]
pub fn review_state(state: State<'_, App>, sheet_id: String) -> CmdResult<TriageStateOut> {
    let id = parse_hex16(&sheet_id).ok_or_else(|| CmdError {
        message: format!("bad sheet id: {sheet_id}"),
    })?;
    let guard = state.lock()?;
    let Some(lib) = guard.as_ref() else {
        return Err(CmdError {
            message: "no project is open".into(),
        });
    };
    Ok(review_session(lib, id)?.state())
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
        sheet_plan,
        sheet_csv_preview,
        sheet_export,
        review_run,
        review_apply,
        review_undo,
        review_export,
        review_state,
    ])
}

/// Registration bookkeeping: the handler list and the `#[tauri::command]`
/// attributes must agree.
///
/// CI's Windows leg is the only place this crate compiles, so a command that
/// loses its attribute (or is never registered) would otherwise only fail after
/// a full push/compile cycle — and with a macro error that names a command
/// nobody deleted on purpose. This test reads the source file itself.
#[cfg(test)]
mod tests {
    /// Command names in the `tauri::generate_handler![…]` list.
    fn registered(source: &str) -> Vec<String> {
        let start = source
            .find("generate_handler![")
            .expect("the handler list exists");
        let rest = &source[start..];
        let end = rest.find("])").expect("the handler list closes");
        rest[..end]
            .split(|c: char| c == ',' || c == '[' || c.is_whitespace())
            // Identifiers only: this drops the `generate_handler!` token itself
            // and the `tauri::` path prefix.
            .filter(|t| !t.is_empty() && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
            .map(str::to_string)
            .collect()
    }

    /// True when the named item is a `pub fn` directly preceded by the command
    /// attribute.
    ///
    /// `pub async fn` counts: an asynchronous command is still a command — it
    /// just runs off the webview's thread — and `review_run` is one, so a test
    /// that only knew `pub fn` would call a properly attributed command
    /// unregistered.
    fn annotated(source: &str, name: &str) -> bool {
        // The `pub` is part of the needle on purpose: it puts the start of the
        // match at the beginning of the signature's line, so the line the
        // attribute check looks at is the line *above* the `fn`.
        let plain = format!("pub fn {name}(");
        let asynchronous = format!("pub async fn {name}(");
        let Some(at) = source.find(&asynchronous).or_else(|| source.find(&plain)) else {
            return false;
        };
        source[..at]
            .lines()
            .rev()
            .find(|l| {
                let l = l.trim();
                !l.is_empty() && !l.starts_with("///") && !l.starts_with("//")
            })
            .is_some_and(|l| l.trim() == "#[tauri::command]")
    }

    #[test]
    fn every_registered_command_is_a_command_function() {
        let source = include_str!("commands.rs");
        let names = registered(source);
        assert!(
            names.len() >= 25,
            "the handler list looks truncated: {names:?}"
        );
        for name in &names {
            assert!(
                annotated(source, name),
                "`{name}` is registered but is not a `#[tauri::command]`"
            );
        }
    }

    #[test]
    fn every_command_function_is_registered() {
        let source = include_str!("commands.rs");
        let names = registered(source);
        let mut annotated = Vec::new();
        for (i, line) in source.lines().enumerate() {
            if line.trim() != "#[tauri::command]" {
                continue;
            }
            let Some(next) = source.lines().nth(i + 1) else {
                continue;
            };
            let next = next.trim();
            // An asynchronous command is a `pub async fn`; both spellings are
            // collected so the count below is the whole surface.
            let Some(rest) = next
                .strip_prefix("pub fn ")
                .or_else(|| next.strip_prefix("pub async fn "))
            else {
                continue;
            };
            let name = rest.split('(').next().unwrap_or_default().to_string();
            annotated.push(name);
        }
        for name in &annotated {
            assert!(
                names.contains(name),
                "`{name}` is a command but missing from the handler list"
            );
        }
        assert_eq!(annotated.len(), names.len(), "counts must match");
    }
}
