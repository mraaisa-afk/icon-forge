/**
 * Typed bridge to the Tauri shell.
 *
 * When running inside the Tauri webview, commands go through
 * `@tauri-apps/api/core.invoke` and job events through
 * `@tauri-apps/api/event.listen("job://event")`.
 *
 * In a plain browser (vite dev server without the shell) the bridge falls
 * back to an in-memory mock implementing the same surface, so the UI can be
 * developed and smoke-tested without the native backend.
 */

/** A sheet-space box, as the commands take it (readonly: the UI's `Bbox`). */
export type Box4 = readonly [number, number, number, number];

import type { GroupingDto, SensitivityDto, SheetPreviewDto, SplitHereDto } from "./groupModel";
import {
  parseAction,
  TRIAGE_ACTIONS,
  type ReviewExportDto,
  type ReviewExportRequestDto,
  type ReviewIconDto,
  type ReviewOutDto,
  type ReviewTriageDto,
  type ReviewUndoDto,
  type TriageActionName,
} from "./reviewModel";
import {
  clampSpec,
  cv,
  DEFAULT_CSV,
  DEFAULT_NAME_PATTERN,
  expandPattern,
  slugify,
  solveGrid,
  targetInk,
  type SheetCsvDto,
  type SheetCsvPreviewDto,
  type SheetExportDto,
  type SheetExportRequest,
  type SheetFileDto,
  type SheetPlanDto,
  type SheetPlanRequest,
  type SheetPlacementDto,
  type SheetReportDto,
} from "./sheetModel";

export interface ProjectInfo {
  path: string;
  sheetCount: number;
  iconCount: number;
}

export interface SheetDto {
  id: string;
  sourcePath: string;
  contentHash: string;
  width: number;
  height: number;
  importedAt: string;
}

export interface StatsDto {
  sheets: number;
  icons: number;
}

export interface ScoreDto {
  mae: number;
  ssim: number;
  iou: number;
  composite: number;
}

/** A stored icon row (comparator grid / review list). */
export interface IconDto {
  /** 32-char hex of the 16-byte icon id. */
  id: string;
  /** Tight bbox `[x, y, w, h]` inside the sheet. */
  bbox: [number, number, number, number];
  /** Stage 8 cache key (empty when the payload is not cached). */
  svgKey: string;
  /** Section 3.3-5 preset doc name. */
  preset: string;
  mae: number;
  ssim: number;
  iou: number;
}

/** One icon's vectorized SVG + score (comparator B-side). */
export interface IconSvgDto {
  svg: string;
  /** True when served from the stage 8 cache. */
  cached: boolean;
  score: ScoreDto;
}

export type JobEvent =
  | { kind: "started"; id: number; name: string }
  | { kind: "progress"; id: number; done: number; total: number; message: string }
  | {
      kind: "finished";
      id: number;
      outcome: "succeeded" | "cancelled" | "preempted" | "failed";
      message?: string;
    };

type Listener = (e: JobEvent) => void;

interface Backend {
  projectOpen(path: string): Promise<ProjectInfo>;
  projectCreate(path: string): Promise<ProjectInfo>;
  projectClose(): Promise<void>;
  projectSave(): Promise<ProjectInfo>;
  importSubmit(root: string): Promise<number>;
  jobCancel(id: number): Promise<boolean>;
  librarySheets(offset: number, limit: number): Promise<SheetDto[]>;
  libraryStats(): Promise<StatsDto>;
  onJobEvent(listener: Listener): Promise<() => void>;
  /** Icons already vectorized (and stored) for one sheet. */
  sheetIcons(sheetId: string): Promise<IconDto[]>;
  /** Base64 PNG of one icon's crop from the normalized sheet. */
  sheetCrop(sheetId: string, x: number, y: number, w: number, h: number): Promise<string>;
  /** Vectorize (or cache-serve) one icon; returns its SVG + score. */
  vectorizeIcon(
    sheetId: string,
    x: number,
    y: number,
    w: number,
    h: number,
    preset: string,
  ): Promise<IconSvgDto>;
  /** Submit the T2 batch vectorization job for one sheet. */
  vectorizeSheetSubmit(sheetId: string, preset: string): Promise<number>;
  /** Group All: groups every icon on the sheet (W12 overlay). */
  groupAll(sheetId: string): Promise<GroupingDto>;
  /** Moves the sensitivity sliders and re-groups from the cached mask. */
  groupSetSensitivity(sheetId: string, sensitivity: SensitivityDto): Promise<GroupingDto>;
  /** Split Here: splits the group under one point (≤ 20 ms budget). */
  groupSplitHere(sheetId: string, x: number, y: number): Promise<SplitHereDto>;
  /** Group Selected: the marquee's groups collapse into one icon. */
  groupSelected(sheetId: string, boxes: readonly Box4[]): Promise<GroupingDto>;
  /** Downscaled PNG of the normalized sheet — the overlay backdrop. */
  sheetPreview(sheetId: string, maxDim: number): Promise<SheetPreviewDto>;
  /** Plans a sheet (§3.5 layout + leveling + the derived metadata). */
  sheetPlan(request: SheetPlanRequest): Promise<SheetPlanDto>;
  /** The CSV the wizard would write, without writing anything. */
  sheetCsvPreview(request: SheetPlanRequest, csv: SheetCsvDto): Promise<SheetCsvPreviewDto>;
  /** Writes the sheet's files and reports the second reader each one passed. */
  sheetExport(request: SheetExportRequest): Promise<SheetExportDto>;

  // Phase 6 review (§3.6): the pass, the triage decisions, `review.csv`.
  /** Runs the review pass over one sheet (seconds of work on a real sheet). */
  reviewRun(sheetId: string): Promise<ReviewOutDto>;
  /** Records one decision and answers with the log's new state. */
  reviewApply(
    sheetId: string,
    iconId: string,
    action: TriageActionName | "bulk" | string,
  ): Promise<ReviewTriageDto>;
  /** Takes back the most recent decision; `null` when there is none left. */
  reviewUndo(sheetId: string): Promise<ReviewUndoDto | null>;
  /** Renders `review.csv`, writing it when `outDir` is given. */
  reviewExport(request: ReviewExportRequestDto): Promise<ReviewExportDto>;
  /** The triage state alone — what the workspace re-reads after a reload. */
  reviewState(sheetId: string): Promise<ReviewTriageDto>;
}

// ---- Tauri backend -------------------------------------------------------

async function tauriBackend(): Promise<Backend> {
  const { invoke } = await import("@tauri-apps/api/core");
  const { listen } = await import("@tauri-apps/api/event");
  return {
    async projectOpen(path) {
      return invoke<ProjectInfo>("project_open", { request: { path } });
    },
    async projectCreate(path) {
      return invoke<ProjectInfo>("project_create", { request: { path } });
    },
    async projectClose() {
      await invoke("project_close");
    },
    async projectSave() {
      return invoke<ProjectInfo>("project_save");
    },
    async importSubmit(root) {
      return invoke<number>("import_submit", { root });
    },
    async jobCancel(id) {
      return invoke<boolean>("job_cancel", { id });
    },
    async librarySheets(offset, limit) {
      return invoke<SheetDto[]>("library_sheets", { offset, limit });
    },
    async libraryStats() {
      return invoke<StatsDto>("library_stats");
    },
    async onJobEvent(listener) {
      const unlisten = await listen<JobEvent>("job://event", (e) => listener(e.payload));
      return unlisten;
    },
    async sheetIcons(sheetId) {
      return invoke<IconDto[]>("sheet_icons", { sheetId });
    },
    async sheetCrop(sheetId, x, y, w, h) {
      return invoke<string>("sheet_crop", { sheetId, x, y, w, h });
    },
    async vectorizeIcon(sheetId, x, y, w, h, preset) {
      return invoke<IconSvgDto>("vectorize_icon", { sheetId, x, y, w, h, preset });
    },
    async vectorizeSheetSubmit(sheetId, preset) {
      return invoke<number>("vectorize_sheet_submit", { request: { sheetId, preset } });
    },
    async groupAll(sheetId) {
      return invoke<GroupingDto>("group_all", { sheetId });
    },
    async groupSetSensitivity(sheetId, sensitivity) {
      return invoke<GroupingDto>("group_set_sensitivity", { sheetId, sensitivity });
    },
    async groupSplitHere(sheetId, x, y) {
      return invoke<SplitHereDto>("group_split_here", { sheetId, x, y });
    },
    async groupSelected(sheetId, boxes) {
      return invoke<GroupingDto>("group_selected", { request: { sheetId, boxes } });
    },
    async sheetPreview(sheetId, maxDim) {
      return invoke<SheetPreviewDto>("sheet_preview", { sheetId, maxDim });
    },
    async sheetPlan(request) {
      return invoke<SheetPlanDto>("sheet_plan", { req: request });
    },
    async sheetCsvPreview(request, csv) {
      return invoke<SheetCsvPreviewDto>("sheet_csv_preview", { req: request, csv });
    },
    async sheetExport(request) {
      return invoke<SheetExportDto>("sheet_export", { req: request });
    },
    // Phase 6. The action travels as the string the command parses, so `A` and
    // `approve` are both accepted on the native side and the workspace does not
    // have to know which spelling the DTO uses.
    async reviewRun(sheetId) {
      return invoke<ReviewOutDto>("review_run", { sheetId });
    },
    async reviewApply(sheetId, iconId, action) {
      return invoke<ReviewTriageDto>("review_apply", { sheetId, iconId, action });
    },
    async reviewUndo(sheetId) {
      return invoke<ReviewUndoDto | null>("review_undo", { sheetId });
    },
    async reviewExport(request) {
      return invoke<ReviewExportDto>("review_export", { req: request });
    },
    async reviewState(sheetId) {
      return invoke<ReviewTriageDto>("review_state", { sheetId });
    },
  };
}

// ---- Browser mock backend ------------------------------------------------

/** 8×8 grey PNG — stand-in crop for the browser mock. */
const MOCK_PNG =
  "iVBORw0KGgoAAAANSUhEUgAAAAgAAAAICAIAAABLbSncAAAAEUlEQVR4nGM4ce0RVsQwtCQAfeqgAYPP7GUAAAAASUVORK5CYII=";

/** Default sensitivity, mirroring `SensitivityParams` on the native side. */
const MOCK_SENSITIVITY: SensitivityDto = {
  mergeGapFrac: 0.35,
  mergeAreaRatio: 1.75,
  noiseMinArea: 16,
  gridRegularityMin: 0.75,
};

/** Slider envelope, mirroring `SensitivityParams::RANGES`. */
const MOCK_RANGES: Record<keyof SensitivityDto, [number, number]> = {
  mergeGapFrac: [0.05, 1],
  mergeAreaRatio: [1, 8],
  noiseMinArea: [0, 512],
  gridRegularityMin: [0, 1],
};

/**
 * A deterministic 0..1 stream — the browser mock must produce the *same*
 * numbers twice, or a wizard preview would drift on every keystroke.
 */
function mockRandoms(seed: number): () => number {
  let state = seed >>> 0 || 1;
  return () => {
    state = (Math.imul(state, 1664525) + 1013904223) >>> 0;
    return state / 0x1_0000_0000;
  };
}

/** RFC 4180 field quoting, the rule `write_csv` uses. */
function mockField(value: string, delimiter: string): string {
  if (value.includes(delimiter) || value.includes('"') || value.includes("\n")) {
    return `"${value.replace(/"/g, '""')}"`;
  }
  return value;
}

/**
 * The mock's synthetic plan: the real layout arithmetic over a plausible
 * spread of ink sizes, so the panel can be developed and tested in a browser
 * without the native sheet generator.
 */
function mockPlan(request: SheetPlanRequest, count: number): SheetPlanDto {
  const spec = clampSpec(request.spec);
  const grid = solveGrid(spec, count);
  const target = targetInk(spec);
  const rand = mockRandoms(count * 2654435761 + spec.cell);
  const stem = request.sheetStem?.trim() || "sheet";
  const placements: SheetPlacementDto[] = [];
  const inks: number[] = [];
  const strokes: number[] = [];
  for (let i = 0; i < count; i += 1) {
    const index = i + 1;
    const row = Math.floor(i / grid.columns) + 1;
    const col = (i % grid.columns) + 1;
    // ±3 % of ink size (cv ≈ 0.017), and a stroke spread of ±6 %.
    const ink = target * (0.97 + 0.06 * rand());
    inks.push(ink);
    strokes.push(target * 0.1 * (0.94 + 0.12 * rand()));
    const cellX = spec.margin + (i % grid.columns) * (spec.cell + spec.gap);
    const cellY = spec.margin + Math.floor(i / grid.columns) * (spec.cell + spec.gap);
    const name = expandPattern(request.namePattern || DEFAULT_NAME_PATTERN, {
      sheet: stem,
      index,
      row,
      col,
      preset: request.preset,
    });
    const slug = slugify(name);
    // Ink is square in the mock; the real one measures the icon's own box.
    placements.push({
      id: index,
      index,
      row,
      col,
      cellX,
      cellY,
      x: cellX + (spec.cell - ink) / 2,
      y: cellY + (spec.cell - ink) / 2,
      w: ink,
      h: ink,
      scale: ink / target,
      flags: [],
      name,
      slug,
      file: `svg/${slug}.svg`,
    });
  }
  const report: SheetReportDto = {
    inkSizeCv: cv(inks),
    strokeCv: cv(strokes),
    baselineSpread: 0,
    medianStroke: [...strokes].sort((a, b) => a - b)[Math.floor(strokes.length / 2)] ?? 0,
    medianSolidity: 0.985,
    overflowBackoffs: 0,
    strokeClamped: 0,
    solidityClamped: 0,
  };
  return {
    width: grid.width,
    height: grid.height,
    columns: grid.columns,
    rows: grid.rows,
    icons: count,
    cell: spec.cell,
    inkRatio: spec.inkRatio,
    placement: spec.placement,
    report,
    placements,
  };
}

/**
 * One decision in the mock's log. Natively the log is keyed by the sheet *row*
 * and the session maps rows to icon ids; the mock carries both, so a decision
 * can name the id the workspace knows without re-deriving it.
 */
interface MockDecision {
  icon: string;
  action: TriageActionName;
  seq: number;
  atMs: number;
}

/**
 * The browser stand-in for `TriageLog` — and it follows the same rules, because
 * the workspace is developed and tested against it: one *current* decision per
 * row, an undo history that puts back what was there before, sequence numbers
 * that never go backwards, and `review.csv` with the same `seq,id,action,at_ms`
 * columns and RFC 4180 line endings the native export writes.
 */
class MockTriageLog {
  private decisions = new Map<number, MockDecision>();
  /** `(row, what was there before)`, oldest first — `TriageLog::history`. */
  private history: Array<{ row: number; previous: MockDecision | null }> = [];
  private nextSeq = 1;

  get canUndo(): boolean {
    return this.history.length > 0;
  }

  get size(): number {
    return this.decisions.size;
  }

  apply(row: number, icon: string, action: TriageActionName, atMs: number): number {
    const seq = this.nextSeq;
    this.nextSeq += 1;
    const previous = this.decisions.get(row) ?? null;
    this.decisions.set(row, { icon, action, seq, atMs });
    this.history.push({ row, previous });
    return seq;
  }

  undo(): { row: number; decision: MockDecision | null } | null {
    const entry = this.history.pop();
    if (!entry) return null;
    if (entry.previous) this.decisions.set(entry.row, entry.previous);
    else this.decisions.delete(entry.row);
    return { row: entry.row, decision: entry.previous };
  }

  actionOf(row: number): TriageActionName | null {
    return this.decisions.get(row)?.action ?? null;
  }

  /** The export, byte-for-byte what the native `to_csv` would write. */
  csv(): string {
    const rows = [...this.decisions.entries()].sort((a, b) => a[1].seq - b[1].seq);
    let out = "seq,id,action,at_ms\r\n";
    for (const [row, decision] of rows) {
      out += `${decision.seq},${row},${decision.action},${decision.atMs}\r\n`;
    }
    return out;
  }

  state(): ReviewTriageDto {
    const counts: [number, number, number, number, number] = [0, 0, 0, 0, 0];
    let lastRow = -1;
    let last: MockDecision | null = null;
    for (const [row, decision] of this.decisions) {
      counts[TRIAGE_ACTIONS.indexOf(decision.action)] += 1;
      if (!last || decision.seq > last.seq) {
        last = decision;
        lastRow = row;
      }
    }
    return {
      decided: this.decisions.size,
      seq: this.nextSeq,
      counts,
      canUndo: this.canUndo,
      ...(last
        ? {
            last: {
              index: lastRow,
              icon: last.icon,
              action: last.action,
              seq: last.seq,
              atMs: last.atMs,
            },
          }
        : {}),
      csvBytes: this.csv().length,
    };
  }
}

class MockBackend implements Backend {
  private sheets: SheetDto[] = [];
  private icons = new Map<string, IconDto[]>();
  private listeners = new Set<Listener>();
  private nextJob = 1;
  private path: string | null = null;
  /** Group All results, per sheet (the browser stand-in for the mask cache). */
  private grouping = new Map<string, GroupingDto>();
  private groupingSeen = new Set<string>();
  private groupingSensitivity = new Map<string, SensitivityDto>();
  private previews = new Map<string, SheetPreviewDto>();
  /** One triage log per sheet, so the mock keeps its decisions like the file does. */
  private reviewLogs = new Map<string, MockTriageLog>();

  private require(): true {
    if (this.path === null) throw new Error("no project is open (browser mock)");
    return true;
  }

  private emit(e: JobEvent): void {
    for (const l of this.listeners) l(e);
  }

  /** Drops every derived cache — a different project invalidates all of them. */
  private reset(): void {
    this.grouping.clear();
    this.groupingSeen.clear();
    this.groupingSensitivity.clear();
    this.previews.clear();
    this.icons.clear();
    this.reviewLogs.clear();
  }

  async projectOpen(path: string): Promise<ProjectInfo> {
    this.reset();
    this.path = path;
    return { path, sheetCount: this.sheets.length, iconCount: 0 };
  }

  async projectCreate(path: string): Promise<ProjectInfo> {
    this.reset();
    this.sheets = [];
    this.path = path;
    return { path, sheetCount: 0, iconCount: 0 };
  }

  async projectClose(): Promise<void> {
    this.reset();
    this.path = null;
  }

  async projectSave(): Promise<ProjectInfo> {
    this.require();
    return { path: this.path ?? "", sheetCount: this.sheets.length, iconCount: 0 };
  }

  async importSubmit(root: string): Promise<number> {
    this.require();
    const id = this.nextJob++;
    void (async () => {
      this.emit({ kind: "started", id, name: "Import folder" });
      for (let i = 1; i <= 5; i++) {
        await new Promise((r) => setTimeout(r, 120));
        this.emit({ kind: "progress", id, done: i * 20, total: 100, message: "importing (mock)" });
      }
      // Two synthetic sheets, derived from the folder so the same path imports
      // the same sheet ids. The native import reads the folder; a mock that
      // imports nothing leaves every screen behind the library grid (the sheet
      // drawer, the comparator, the review workspace) unreachable in a plain
      // `vite dev` session, which is the one session the mock exists for.
      const dir = root.replace(/[\\/]+$/, "");
      this.sheets = ["sheet_a", "sheet_b"].map((stem, index) => ({
        id: this.mockHash(`${dir}/${stem}`, index) + this.mockHash(stem, index + 1),
        sourcePath: `${dir}/${stem}.png`,
        contentHash: this.mockHash(`${stem}:content`, index) + this.mockHash(dir, index + 7),
        width: 256 + index * 64,
        height: 192 + index * 48,
        importedAt: String(Date.now()),
      }));
      this.emit({
        kind: "finished",
        id,
        outcome: "succeeded",
        message: `mock import from ${root}`,
      });
    })();
    return id;
  }

  async jobCancel(id: number): Promise<boolean> {
    this.emit({ kind: "finished", id, outcome: "cancelled" });
    return true;
  }

  async librarySheets(offset: number, limit: number): Promise<SheetDto[]> {
    this.require();
    return this.sheets.slice(offset, offset + limit);
  }

  async libraryStats(): Promise<StatsDto> {
    this.require();
    return { sheets: this.sheets.length, icons: 0 };
  }

  async sheetIcons(_sheetId: string): Promise<IconDto[]> {
    this.require();
    return this.icons.get(_sheetId) ?? [];
  }

  async sheetCrop(
    _sheetId: string,
    _x: number,
    _y: number,
    _w: number,
    _h: number,
  ): Promise<string> {
    this.require();
    return MOCK_PNG;
  }

  async vectorizeIcon(
    _sheetId: string,
    x: number,
    y: number,
    w: number,
    h: number,
    preset: string,
  ): Promise<IconSvgDto> {
    this.require();
    // Deterministic stand-in scores derived from the bbox.
    const seed = (x * 7 + y * 13 + w * 3 + h * 5) % 100;
    const ssim = 0.94 + seed / 2000;
    return {
      svg:
        `<svg xmlns="http://www.w3.org/2000/svg" width="${w}" height="${h}" ` +
        `viewBox="0 0 ${w} ${h}"><title>icon @ ${x},${y}</title>` +
        `<desc>mock · ${preset}</desc>` +
        `<path fill="#1a1a1a" d="M2 2h${w - 4}v${h - 4}H2Z"/></svg>`,
      cached: seed % 2 === 0,
      score: {
        mae: 0.02 + (seed % 5) / 500,
        ssim,
        iou: 0.9 + (seed % 7) / 100,
        composite: Math.min(1, 0.5 * ssim + 0.3 * 0.97 + 0.2 * 0.95),
      },
    };
  }

  async vectorizeSheetSubmit(sheetId: string, preset: string): Promise<number> {
    this.require();
    const sheet = this.sheets.find((s) => s.id === sheetId);
    const pw = Math.max(8, Math.min(64, Math.floor((sheet?.width ?? 64) / 8)));
    const ph = Math.max(8, Math.min(64, Math.floor((sheet?.height ?? 64) / 8)));
    const cells: Array<[number, number]> = [
      [pw, ph],
      [pw * 3, ph],
      [pw, ph * 3],
      [pw * 3, ph * 3],
    ];
    const rows: IconDto[] = cells.map(([x, y], i) => ({
      id: `${sheetId.slice(0, 24)}${i.toString(16).padStart(8, "0")}`,
      bbox: [x, y, pw, ph],
      svgKey: `mock-${sheetId.slice(0, 8)}-${i}-${preset}`,
      preset,
      mae: 0.02 + i / 200,
      ssim: 0.97 - i / 500,
      iou: 0.94 - i / 200,
    }));
    this.icons.set(sheetId, rows);
    const id = this.nextJob++;
    void (async () => {
      this.emit({ kind: "started", id, name: "Vectorize sheet (mock)" });
      for (let i = 1; i <= 4; i++) {
        await new Promise((r) => setTimeout(r, 100));
        this.emit({ kind: "progress", id, done: i, total: 4, message: "vectorizing (mock)" });
      }
      this.emit({
        kind: "finished",
        id,
        outcome: "succeeded",
        message: `mock vectorize · ${preset}`,
      });
    })();
    return id;
  }

  // ---- W12 grouping ------------------------------------------------------

  /**
   * 4 columns × 3 rows of tiles derived from the sheet size. The merge-area
   * slider is the one knob that visibly changes the mock: past 3× the first two
   * tiles glue into one, which is what the real rule 5 does to a tight pair.
   */
  private syntheticGroups(sheetId: string, sensitivity: SensitivityDto): GroupingDto {
    const sheet = this.sheets.find((s) => s.id === sheetId);
    const width = sheet?.width ?? 256;
    const height = sheet?.height ?? 192;
    const gapX = Math.max(4, Math.round(width / 20));
    const gapY = Math.max(4, Math.round(height / 20));
    const cellW = Math.floor((width - gapX * 5) / 4);
    const cellH = Math.floor((height - gapY * 4) / 3);
    const tiles: Array<[number, number, number, number]> = [];
    for (let row = 0; row < 3; row++) {
      for (let col = 0; col < 4; col++) {
        tiles.push([gapX + col * (cellW + gapX), gapY + row * (cellH + gapY), cellW, cellH]);
      }
    }
    if (sensitivity.mergeAreaRatio >= 3 && tiles.length >= 2) {
      const [a, b] = tiles;
      tiles.splice(0, 2, [a[0], a[1], b[0] + b[2] - a[0], a[3]]);
    }
    const first = this.groupingSeen.has(sheetId);
    const groups = tiles.map((bbox) => ({ bbox, area: bbox[2] * bbox[3] }));
    const review = sensitivity.mergeAreaRatio >= 3 ? 2 : 1;
    return {
      sheetId,
      width,
      height,
      groups,
      warnings: [
        {
          group: Math.min(5, groups.length - 1),
          kind: "spansMultipleCells" as const,
          label: "spans multiple cells (mock)",
        },
        ...(review > 1
          ? [
              {
                group: Math.min(1, groups.length - 1),
                kind: "restoredFromMerge" as const,
                label: "restored from a merge across a valley (mock)",
              },
            ]
          : []),
      ],
      confidence: sensitivity.mergeAreaRatio >= 3 ? 0.9 : 0.94,
      reviewGroups: review,
      statusLine: `Grouped ${groups.length} icons in 0.00 s · confidence ${
        sensitivity.mergeAreaRatio >= 3 ? 90 : 94
      }% · ${review} groups need review`,
      elapsedMs: 4,
      maskCacheHit: first,
      manualEdits: 0,
      sensitivity,
      hint: {
        gridX: true,
        gridY: true,
        cellsX: 4,
        cellsY: 3,
        valleyX: Array.from(
          { length: 3 },
          (_, i) => gapX * (i + 1) + cellW * (i + 1) - Math.round(gapX / 2),
        ),
        valleyY: Array.from(
          { length: 2 },
          (_, i) => gapY * (i + 1) + cellH * (i + 1) - Math.round(gapY / 2),
        ),
      },
      stats: {
        input: tiles.length,
        output: groups.length,
        merges: sensitivity.mergeAreaRatio >= 3 ? 1 : 0,
        restored: 0,
        flagged: review,
        resplit: 0,
        iterations: 0,
        medianH: cellH,
        medianArea: cellW * cellH,
        elapsedMs: 3,
      },
    };
  }

  async groupAll(sheetId: string): Promise<GroupingDto> {
    this.require();
    const sensitivity = this.groupingSensitivity.get(sheetId) ?? { ...MOCK_SENSITIVITY };
    const dto = this.syntheticGroups(sheetId, sensitivity);
    this.groupingSeen.add(sheetId);
    this.grouping.set(sheetId, dto);
    return dto;
  }

  /** The grouping currently on screen, grouped on demand (never re-groups an
   * edited result — that is what `groupAll` is for). */
  private current(sheetId: string): GroupingDto {
    const existing = this.grouping.get(sheetId);
    if (existing) return existing;
    const dto = this.syntheticGroups(
      sheetId,
      this.groupingSensitivity.get(sheetId) ?? { ...MOCK_SENSITIVITY },
    );
    this.groupingSeen.add(sheetId);
    this.grouping.set(sheetId, dto);
    return dto;
  }

  async groupSetSensitivity(sheetId: string, sensitivity: SensitivityDto): Promise<GroupingDto> {
    this.require();
    const clamped = { ...sensitivity };
    for (const key of Object.keys(MOCK_RANGES) as (keyof SensitivityDto)[]) {
      const [lo, hi] = MOCK_RANGES[key];
      const value = Number.isFinite(clamped[key]) ? clamped[key] : lo;
      clamped[key] = Math.max(lo, Math.min(hi, value));
    }
    this.groupingSensitivity.set(sheetId, clamped);
    const dto = this.syntheticGroups(sheetId, clamped);
    // A regroup after the first one is a cache hit by construction.
    const hit: GroupingDto = { ...dto, maskCacheHit: true };
    this.groupingSeen.add(sheetId);
    this.grouping.set(sheetId, hit);
    return hit;
  }

  async groupSplitHere(sheetId: string, x: number, y: number): Promise<SplitHereDto> {
    this.require();
    const current = this.current(sheetId);
    const index = current.groups.findIndex(
      (g) =>
        x >= g.bbox[0] && y >= g.bbox[1] && x < g.bbox[0] + g.bbox[2] && y < g.bbox[1] + g.bbox[3],
    );
    if (index < 0) throw new Error(`no group at (${x}, ${y})`);
    const g = current.groups[index];
    // Small tiles have no watershed structure — the mock refuses those, like
    // the real guards do.
    if (g.bbox[2] < 40) {
      return { split: false, regions: 0, groupIndex: index, elapsedMs: 0.2, report: current };
    }
    const halfW = Math.max(1, Math.floor(g.bbox[2] / 2));
    const groups = [...current.groups];
    groups.splice(
      index,
      1,
      { bbox: [g.bbox[0], g.bbox[1], halfW, g.bbox[3]], area: halfW * g.bbox[3] },
      {
        bbox: [g.bbox[0] + halfW, g.bbox[1], g.bbox[2] - halfW, g.bbox[3]],
        area: (g.bbox[2] - halfW) * g.bbox[3],
      },
    );
    const report: GroupingDto = {
      ...current,
      groups,
      manualEdits: current.manualEdits + 1,
      maskCacheHit: true,
      statusLine: current.statusLine.replace(
        /^Grouped \d+ icons/,
        `Grouped ${groups.length} icons`,
      ),
    };
    this.grouping.set(sheetId, report);
    return { split: true, regions: 2, groupIndex: index, elapsedMs: 0.4, report };
  }

  async groupSelected(sheetId: string, boxes: readonly Box4[]): Promise<GroupingDto> {
    this.require();
    const current = this.current(sheetId);
    const hits = current.groups
      .map((g, i) => ({ g, i }))
      .filter(({ g }) =>
        boxes.some(
          (b) =>
            g.bbox[0] < b[0] + b[2] &&
            b[0] < g.bbox[0] + g.bbox[2] &&
            g.bbox[1] < b[1] + b[3] &&
            b[1] < g.bbox[1] + g.bbox[3],
        ),
      );
    if (hits.length < 2) return current;
    const x0 = Math.min(...hits.map((h) => h.g.bbox[0]));
    const y0 = Math.min(...hits.map((h) => h.g.bbox[1]));
    const x1 = Math.max(...hits.map((h) => h.g.bbox[0] + h.g.bbox[2]));
    const y1 = Math.max(...hits.map((h) => h.g.bbox[1] + h.g.bbox[3]));
    const groups = current.groups.filter((_, i) => !hits.some((h) => h.i === i));
    groups.push({
      bbox: [x0, y0, x1 - x0, y1 - y0],
      area: hits.reduce((a, h) => a + h.g.area, 0),
    });
    const report: GroupingDto = {
      ...current,
      groups,
      manualEdits: current.manualEdits + 1,
      maskCacheHit: true,
      statusLine: current.statusLine.replace(
        /^Grouped \d+ icons/,
        `Grouped ${groups.length} icons`,
      ),
    };
    this.grouping.set(sheetId, report);
    return report;
  }

  async sheetPreview(sheetId: string, maxDim: number): Promise<SheetPreviewDto> {
    this.require();
    const cacheKey = `${sheetId}:${maxDim}`;
    const cached = this.previews.get(cacheKey);
    if (cached) return cached;
    const sheet = this.sheets.find((s) => s.id === sheetId);
    const sheetWidth = sheet?.width ?? 256;
    const sheetHeight = sheet?.height ?? 192;
    const longest = Math.max(sheetWidth, sheetHeight);
    const scale = longest > maxDim ? maxDim / longest : 1;
    const preview: SheetPreviewDto = {
      png: MOCK_PNG,
      width: Math.max(1, Math.round(sheetWidth * scale)),
      height: Math.max(1, Math.round(sheetHeight * scale)),
      sheetWidth,
      sheetHeight,
    };
    this.previews.set(cacheKey, preview);
    return preview;
  }

  /** How many icons the mock plans for a sheet: what Group All found, else a grid. */
  private mockCount(sheetId: string): number {
    return this.grouping.get(sheetId)?.groups.length ?? 16;
  }

  async sheetPlan(request: SheetPlanRequest): Promise<SheetPlanDto> {
    this.require();
    return mockPlan(request, this.mockCount(request.sheetId));
  }

  async sheetCsvPreview(request: SheetPlanRequest, csv: SheetCsvDto): Promise<SheetCsvPreviewDto> {
    this.require();
    const plan = mockPlan(request, this.mockCount(request.sheetId));
    const options = { ...DEFAULT_CSV, ...csv };
    const columns = options.columns.filter((c) => c.length > 0);
    const rows = plan.placements.map((p) => {
      const values: Record<string, string> = {
        index: String(p.index),
        name: p.name,
        slug: p.slug,
        tags: request.preset,
        file: p.file,
        row: String(p.row),
        col: String(p.col),
        width: String(Math.round(p.w)),
        height: String(Math.round(p.h)),
        ink: p.w.toFixed(2),
        stroke: plan.report.medianStroke.toFixed(2),
        solidity: plan.report.medianSolidity.toFixed(3),
        area: String(Math.round(p.w * p.h)),
        preset: request.preset,
        colours: "1",
        id: p.id.toString(16).padStart(8, "0"),
      };
      return columns.map((c) => values[c] ?? "");
    });
    const lines = rows.map((r) =>
      r.map((v) => mockField(v, options.delimiter)).join(options.delimiter),
    );
    const text = [...(options.header ? [columns.join(options.delimiter)] : []), ...lines].join(
      "\r\n",
    );
    return { columns: [...columns], rows, text: `${text}\r\n` };
  }

  async sheetExport(request: SheetExportRequest): Promise<SheetExportDto> {
    this.require();
    const count = this.mockCount(request.sheetId);
    const plan = mockPlan(request, count);
    const stem = request.sheetStem?.trim() || "sheet";
    const formats = request.formats.length > 0 ? request.formats : ["svg", "pdf", "png", "csv"];
    const dir = request.outDir.replace(/[\\/]+$/, "");
    const files: SheetFileDto[] = [];
    for (const format of formats) {
      const paths: Record<string, string> = {
        svg: `${dir}/${stem}-sheet.svg`,
        pdf: `${dir}/${stem}-sheet.pdf`,
        png: `${dir}/${stem}-sheet.png`,
        csv: `${dir}/${stem}-icons.csv`,
        icons: `${dir}/svg`,
      };
      if (!(format in paths)) throw new Error(`unknown export format: ${format}`);
      // Plausible sizes: ~180 B per placed icon for the vector files, four
      // bytes per pixel for the raster, and one CSV line each.
      const bytes =
        format === "icons"
          ? 0
          : format === "png"
            ? plan.width * plan.height * 4
            : format === "csv"
              ? count * 96
              : count * 180;
      files.push({
        format,
        path: paths[format],
        bytes,
        evidence: `mock: ${format} written for ${count} icons (no native sheet generator in the browser)`,
      });
    }
    return { plan, files };
  }

  // ---- Phase 6 review ----------------------------------------------------

  /** The log for a sheet, created on first use like the table's empty journal. */
  private reviewLog(sheetId: string): MockTriageLog {
    let log = this.reviewLogs.get(sheetId);
    if (!log) {
      log = new MockTriageLog();
      this.reviewLogs.set(sheetId, log);
    }
    return log;
  }

  /**
   * 16 hex chars derived from a string — the mock's stand-in for a hash.
   *
   * Both halves are forced through `>>> 0`: JavaScript's bitwise operators work
   * on *signed* 32-bit integers, so `state ^ salt` can come back negative and
   * `toString(16)` then renders `-8e86732` — a 31-character id, which is not a
   * 32-hex id at all.
   */
  private mockHash(text: string, salt: number): string {
    let state = (0x811c9dc5 ^ salt) >>> 0;
    for (let i = 0; i < text.length; i += 1) {
      state = Math.imul(state ^ text.charCodeAt(i), 0x01000193) >>> 0;
    }
    const other = (state ^ 0x5bf03635) >>> 0;
    return state.toString(16).padStart(8, "0") + other.toString(16).padStart(8, "0");
  }

  /**
   * The mock's review pass over the rows `vectorizeSheetSubmit` left behind.
   *
   * It is synthetic but it is *shaped* like the real report: a duplicate cluster
   * whose two members share both hashes and a digest, one icon with an
   * open-contour flag and a stroke deviation, one over-complex pair of numbers,
   * and a skip. The workspace is developed against these, so the mock has to
   * exercise every branch the real detector output can take.
   */
  private mockReviewRows(sheetId: string): ReviewIconDto[] {
    const rows = this.icons.get(sheetId) ?? [];
    const log = this.reviewLog(sheetId);
    return rows.map((row, i) => {
      const [, , w] = row.bbox;
      const twin = i === 0 || i === 1;
      const digest = twin ? this.mockHash(sheetId, 7) + "0".repeat(32) : this.mockHash(sheetId, 7 + i) + "1".repeat(32);
      const dHash = twin ? this.mockHash(sheetId, 11) : this.mockHash(`${row.id}`, 11);
      const aHash = twin ? this.mockHash(sheetId, 13) : this.mockHash(`${row.id}`, 13);
      return {
        id: row.id,
        index: i,
        score: {
          mae: row.mae,
          ssim: row.ssim,
          iou: row.iou,
          composite: Math.min(1, 0.5 * row.ssim + 0.3 * row.iou + 0.2 * (1 - row.mae)),
        },
        flags: i === 2 ? ["open-contour"] : i === 3 ? ["over-complex"] : [],
        nodeCount: 8 + i * 4,
        closed: i !== 2,
        colours: 1 + (i % 2),
        inkArea: Math.max(1, w * (row.bbox[3] ?? 1)),
        stat: {
          inkSize: Math.max(1, w),
          stroke: Math.max(0.5, w / 10),
          nodeCount: 8 + i * 4,
          colours: 1 + (i % 2),
          solidity: 0.98 - i / 100,
          fillRatio: twin ? 1 : 0.94 - i / 50,
          palette: this.mockHash(`${row.id}:palette`, 17) + "0".repeat(16),
        },
        dHash,
        aHash,
        digest,
        state: log.actionOf(i) ?? "pending",
        ...(twin
          ? {
              cluster: {
                members: rows
                  .slice(0, 2)
                  .map((r) => r.id)
                  .sort(),
                keeper: rows[1].id,
                identical: true,
              },
            }
          : {}),
        keeper: twin && i === 1,
        outliers:
          i === 2
            ? [{ kind: "stroke", z: 3.9, value: Math.max(1, w / 10) * 1.8, median: Math.max(1, w / 10) }]
            : [],
      };
    });
  }

  async reviewRun(sheetId: string): Promise<ReviewOutDto> {
    this.require();
    const icons = this.mockReviewRows(sheetId);
    const log = this.reviewLog(sheetId);
    // The deviations again, sheet-level, with the icon each belongs to.
    const outliers = icons.flatMap((icon) =>
      icon.outliers.map((flag) => ({ icon: icon.id, ...flag })),
    );
    return {
      sheet: sheetId,
      icons,
      clusters: icons.flatMap((icon) => (icon.cluster && icon.keeper ? [icon.cluster] : [])),
      outliers,
      skipped: icons.length === 0 ? [{ id: "0".repeat(32), reason: "the sheet has no icons yet (mock)" }] : [],
      flagged: icons.filter((icon) => icon.flags.length > 0).length,
      cascade: {
        candidates: icons.filter((icon) => icon.cluster).length * 3,
        verified: icons.filter((icon) => icon.cluster).length * 2,
        confirmed: icons.filter((icon) => icon.cluster).length,
      },
      renderMs: icons.length * 0.9,
      detectMs: icons.length * 2.4,
      triage: log.state(),
    };
  }

  async reviewApply(
    sheetId: string,
    iconId: string,
    action: string,
  ): Promise<ReviewTriageDto> {
    this.require();
    const parsed = parseAction(action);
    if (!parsed) throw new Error(`unknown triage action: ${action}`);
    const icons = this.mockReviewRows(sheetId);
    const row = icons.findIndex((icon) => icon.id === iconId);
    if (row === -1) throw new Error(`icon ${iconId} is not part of sheet ${sheetId}`);
    this.reviewLog(sheetId).apply(row, iconId, parsed, Date.now());
    return this.reviewLog(sheetId).state();
  }

  async reviewUndo(sheetId: string): Promise<ReviewUndoDto | null> {
    this.require();
    const log = this.reviewLog(sheetId);
    const undone = log.undo();
    if (!undone) return null;
    const rows = this.icons.get(sheetId) ?? [];
    const icon = rows[undone.row]?.id ?? undone.decision?.icon ?? "";
    return {
      triage: log.state(),
      icon,
      restored: undone.decision?.action ?? "pending",
    };
  }

  async reviewExport(request: ReviewExportRequestDto): Promise<ReviewExportDto> {
    this.require();
    const log = this.reviewLog(request.sheetId);
    const csv = log.csv();
    const dir = request.outDir?.replace(/[\\/]+$/, "");
    return {
      csv,
      decisions: log.size,
      seq: log.state().seq,
      ...(dir ? { path: `${dir}/review-${request.sheetId}.csv` } : {}),
    };
  }

  async reviewState(sheetId: string): Promise<ReviewTriageDto> {
    this.require();
    return this.reviewLog(sheetId).state();
  }

  async onJobEvent(listener: Listener): Promise<() => void> {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  }
}

// ---- Selection -----------------------------------------------------------

let backendPromise: Promise<Backend> | null = null;

/** True when running inside the Tauri webview. */
export function hasTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

/** Resolves the active backend exactly once. */
export function backend(): Promise<Backend> {
  backendPromise ??= hasTauri() ? tauriBackend() : Promise.resolve(new MockBackend());
  return backendPromise;
}
