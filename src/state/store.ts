/**
 * Zustand store: project state, library page cache, and job status.
 * All backend access flows through here — components stay declarative.
 */

import { create } from "zustand";
import { backend, type JobEvent, type ProjectInfo, type SheetDto, type StatsDto } from "../lib/backend";

export interface JobStatus {
  id: number;
  name: string;
  /** null = still running. */
  outcome: "succeeded" | "cancelled" | "preempted" | "failed" | null;
  message: string;
  done: number;
  total: number;
}

export interface UiState {
  project: ProjectInfo | null;
  sheets: SheetDto[];
  totalCount: number;
  jobs: Record<number, JobStatus>;
  error: string | null;
  busy: boolean;

  openProject: (path: string) => Promise<void>;
  createProject: (path: string) => Promise<void>;
  closeProject: () => Promise<void>;
  saveProject: () => Promise<void>;
  importFolder: (root: string) => Promise<void>;
  cancelJob: (id: number) => Promise<void>;
  refreshPage: (offset: number, limit: number) => Promise<void>;
  startEventPump: () => void;
}

function jobFromEvent(e: JobEvent): JobStatus | null {
  switch (e.kind) {
    case "started":
      return { id: e.id, name: e.name, outcome: null, message: "", done: 0, total: 0 };
    case "progress":
      return {
        id: e.id,
        name: "",
        outcome: null,
        message: e.message,
        done: e.done,
        total: e.total,
      };
    case "finished":
      return {
        id: e.id,
        name: "",
        outcome: e.outcome,
        message: e.message ?? "",
        done: 0,
        total: 0,
      };
  }
}

export const useStore = create<UiState>((set, get) => ({
  project: null,
  sheets: [],
  totalCount: 0,
  jobs: {},
  error: null,
  busy: false,

  async openProject(path) {
    set({ busy: true, error: null });
    try {
      const project = await backend().then((b) => b.projectOpen(path));
      set({ project });
      await get().refreshPage(0, 200);
    } catch (e) {
      set({ error: String(e) });
    } finally {
      set({ busy: false });
    }
  },

  async createProject(path) {
    set({ busy: true, error: null });
    try {
      const project = await backend().then((b) => b.projectCreate(path));
      set({ project, sheets: [], totalCount: 0 });
    } catch (e) {
      set({ error: String(e) });
    } finally {
      set({ busy: false });
    }
  },

  async closeProject() {
    try {
      await backend().then((b) => b.projectClose());
      set({ project: null, sheets: [], totalCount: 0 });
    } catch (e) {
      set({ error: String(e) });
    }
  },

  async saveProject() {
    set({ busy: true, error: null });
    try {
      const project = await backend().then((b) => b.projectSave());
      set({ project });
    } catch (e) {
      set({ error: String(e) });
    } finally {
      set({ busy: false });
    }
  },

  async importFolder(root) {
    try {
      await backend().then((b) => b.importSubmit(root));
    } catch (e) {
      set({ error: String(e) });
    }
  },

  async cancelJob(id) {
    try {
      await backend().then((b) => b.jobCancel(id));
    } catch (e) {
      set({ error: String(e) });
    }
  },

  async refreshPage(offset, limit) {
    const b = await backend();
    try {
      const sheets = await b.librarySheets(offset, limit);
      const stats: StatsDto | null = get().project ? await b.libraryStats() : null;
      set({ sheets, totalCount: stats?.sheets ?? sheets.length });
    } catch (e) {
      set({ error: String(e) });
    }
  },

  startEventPump() {
    void (async () => {
      const b = await backend();
      b.onJobEvent((e) => {
        const patch = jobFromEvent(e);
        if (!patch) return;
        const jobs = { ...get().jobs };
        const prev = jobs[e.id];
        jobs[e.id] = {
          id: e.id,
          name: prev?.name ?? patch.name,
          outcome: patch.outcome ?? prev?.outcome ?? null,
          message: patch.message || prev?.message || "",
          done: patch.done || prev?.done || 0,
          total: patch.total || prev?.total || 0,
        };
        set({ jobs });
        // When an import finishes, refresh the visible page.
        if (e.kind === "finished" && e.outcome === "succeeded") {
          void get().refreshPage(0, 200);
        }
      });
    })();
  },
}));
