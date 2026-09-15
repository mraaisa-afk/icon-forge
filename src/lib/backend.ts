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
  | { kind: "finished"; id: number; outcome: "succeeded" | "cancelled" | "preempted" | "failed"; message?: string };

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
  };
}

// ---- Browser mock backend ------------------------------------------------

/** 8×8 grey PNG — stand-in crop for the browser mock. */
const MOCK_PNG =
  "iVBORw0KGgoAAAANSUhEUgAAAAgAAAAICAIAAABLbSncAAAAEUlEQVR4nGM4ce0RVsQwtCQAfeqgAYPP7GUAAAAASUVORK5CYII=";

class MockBackend implements Backend {
  private sheets: SheetDto[] = [];
  private icons = new Map<string, IconDto[]>();
  private listeners = new Set<Listener>();
  private nextJob = 1;
  private path: string | null = null;

  private require(): true {
    if (this.path === null) throw new Error("no project is open (browser mock)");
    return true;
  }

  private emit(e: JobEvent): void {
    for (const l of this.listeners) l(e);
  }

  async projectOpen(path: string): Promise<ProjectInfo> {
    this.path = path;
    return { path, sheetCount: this.sheets.length, iconCount: 0 };
  }

  async projectCreate(path: string): Promise<ProjectInfo> {
    this.sheets = [];
    this.path = path;
    return { path, sheetCount: 0, iconCount: 0 };
  }

  async projectClose(): Promise<void> {
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
      this.emit({ kind: "finished", id, outcome: "succeeded", message: `mock import from ${root}` });
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

  async sheetCrop(_sheetId: string, _x: number, _y: number, _w: number, _h: number): Promise<string> {
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
      this.emit({ kind: "finished", id, outcome: "succeeded", message: `mock vectorize · ${preset}` });
    })();
    return id;
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
