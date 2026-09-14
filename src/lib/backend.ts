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
  };
}

// ---- Browser mock backend ------------------------------------------------

class MockBackend implements Backend {
  private sheets: SheetDto[] = [];
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
