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
