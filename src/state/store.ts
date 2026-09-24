/**
 * Zustand store: project state, library page cache, and job status.
 * All backend access flows through here — components stay declarative.
 */

import { create } from "zustand";
import {
  backend,
  type IconDto,
  type IconSvgDto,
  type JobEvent,
  type ProjectInfo,
  type SheetDto,
  type StatsDto,
} from "../lib/backend";
import { iconKey, type Bbox, type ViewMode } from "../lib/compareModel";
import {
  clampSensitivity,
  type GroupingDto,
  type SensitivityDto,
  type SheetPreviewDto,
} from "../lib/groupModel";
import {
  filterRows,
  formatBytes,
  nextRow,
  orderRows,
  patchState,
  stateAfter,
  type ReviewExportDto,
  type ReviewFilter,
  type ReviewOrder,
  type ReviewOutDto,
  type ReviewTriageDto,
  type TriageActionName,
} from "../lib/reviewModel";
import type {
  SheetCsvDto,
  SheetCsvPreviewDto,
  SheetExportRequest,
  SheetFileDto,
  SheetPlanDto,
  SheetPlanRequest,
} from "../lib/sheetModel";

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

  // Sheet detail + A/B comparator (W5).
  selectedSheet: SheetDto | null;
  icons: IconDto[];
  sheetBusy: boolean;
  /** The icon under comparison and the preset it was requested with. */
  comparing: { bbox: Bbox; preset: string } | null;
  /** vectorized results memoized by `iconKey(bbox, preset)`. */
  vectorized: Record<string, IconSvgDto>;
  view: ViewMode;
  wipe: number;

  // Group All overlay (W12).
  /** The current grouping report for `selectedSheet`, or null before Group All. */
  grouping: GroupingDto | null;
  /** The overlay backdrop (downscaled sheet PNG). */
  preview: SheetPreviewDto | null;
  /** True while a grouping command is in flight. */
  groupBusy: boolean;
  /** Last Split Here / Group Selected outcome, shown under the overlay. */
  groupNote: string | null;
  /** Sliders the UI is showing; the backend echoes what it actually used. */
  sensitivity: SensitivityDto | null;

  // Sheet generator (§3.5) — the plan, the CSV preview and the last export.
  /** The last plan for `selectedSheet`, or null before the wizard plans. */
  sheetPlan: SheetPlanDto | null;
  /** The CSV the wizard would write, as the backend re-read it. */
  sheetCsv: SheetCsvPreviewDto | null;
  /** The files the last export wrote, with each one's evidence. */
  sheetFiles: SheetFileDto[] | null;
  /** True while a sheet command is in flight. */
  sheetGenBusy: boolean;
  /** What the last sheet command did, shown under the wizard. */
  sheetNote: string | null;

  openProject: (path: string) => Promise<void>;
  createProject: (path: string) => Promise<void>;
  closeProject: () => Promise<void>;
  saveProject: () => Promise<void>;
  importFolder: (root: string) => Promise<void>;
  cancelJob: (id: number) => Promise<void>;
  refreshPage: (offset: number, limit: number) => Promise<void>;
  startEventPump: () => void;
  openSheet: (sheet: SheetDto) => Promise<void>;
  closeSheet: () => void;
  reloadIcons: () => Promise<void>;
  vectorizeSheet: () => Promise<void>;
  selectIcon: (bbox: Bbox) => Promise<void>;
  setPreset: (preset: string) => Promise<void>;
  setView: (view: ViewMode) => void;
  setWipe: (wipe: number) => void;
  /** Fetches (or serves from memo) the SVG+score for one icon+preset. */
  ensureIcon: (bbox: Bbox, preset: string) => Promise<void>;

  /** Group All over the open sheet (loads the preview too, once). */
  groupAll: () => Promise<void>;
  /** Moves one sensitivity slider and re-groups from the cached mask. */
  setSensitivity: (patch: Partial<SensitivityDto>) => Promise<void>;
  /** Split Here at a sheet-space point. */
  splitHere: (x: number, y: number) => Promise<void>;
  /** Group Selected: merges every group the marquee boxes hit. */
  groupSelected: (boxes: Bbox[]) => Promise<void>;
  /** Drops manual edits by re-grouping from the cached mask. */
  resetGrouping: () => Promise<void>;
  /** Loads the overlay backdrop for the open sheet. */
  loadPreview: () => Promise<void>;
  /** Clears the overlay when the sheet changes. */
  clearGrouping: () => void;

  /** Plans the sheet: layout, leveling and the derived metadata. */
  planSheet: (request: SheetPlanRequest) => Promise<void>;
  /** Previews the CSV the wizard would write (nothing is written). */
  previewSheetCsv: (request: SheetPlanRequest, csv: SheetCsvDto) => Promise<void>;
  /** Writes the sheet's files into `request.outDir`. */
  exportSheet: (request: SheetExportRequest) => Promise<void>;
  /** Drops the plan when the wizard's controls change. */
  clearSheetPlan: () => void;

  // Review workspace (§3.6) — the pass, the triage log and the export.
  /** The last review pass for `selectedSheet`, or null before one runs. */
  review: ReviewOutDto | null;
  /** The sheet's triage log as `review_state` reads it — no pass required. */
  reviewLog: ReviewTriageDto | null;
  /** True while the workspace is showing (it takes the main area). */
  reviewOpen: boolean;
  /** True while a review command is in flight. */
  reviewBusy: boolean;
  /** What the last review command did, shown under the list. */
  reviewNote: string | null;
  /** The filter tab and the list order the reviewer chose. */
  reviewFilter: ReviewFilter;
  reviewOrder: ReviewOrder;
  /** The cursor: an icon id, so it survives re-filtering and re-ordering. */
  reviewSelected: string | null;
  /** The icon whose sheet crop is pinned as the overlay, and that crop. */
  reviewOverlay: string | null;
  reviewCrop: string | null;
  /** When this session's first pass ran — the pace line's clock. */
  reviewStartedAt: number;
  /** The last export, so the workspace can show the bytes and the path. */
  reviewExported: ReviewExportDto | null;

  /** Opens the workspace, reusing the cached pass unless `force` is set. */
  openReview: (force?: boolean) => Promise<void>;
  /** Closes the workspace (the pass and its decisions stay in the log). */
  closeReview: () => void;
  setReviewFilter: (filter: ReviewFilter) => void;
  setReviewOrder: (order: ReviewOrder) => void;
  /** Moves the cursor, skipping decided rows by default. */
  moveReviewSelection: (delta: 1 | -1, skipDecided?: boolean) => void;
  selectReviewIcon: (iconId: string) => void;
  /** One triage decision; the backend's log is the authority, the row is patched. */
  decideReview: (iconId: string, action: TriageActionName) => Promise<void>;
  /** `Shift+A`: approves every row still undecided, one decision each. */
  bulkApproveReview: () => Promise<void>;
  /** `Ctrl+Z`: takes back the most recent decision. */
  undoReview: () => Promise<void>;
  /** Space: pins (or unpins) the sheet crop of one icon as the overlay. */
  toggleReviewOverlay: (iconId?: string) => Promise<void>;
  /** Re-reads the triage state — what a reload or a second window needs. */
  refreshReviewState: () => Promise<void>;
  /** Renders `review.csv`; without `outDir` it is a preview. */
  exportReview: (outDir?: string) => Promise<void>;
  /** Drops the workspace state when the sheet changes. */
  clearReview: () => void;
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

  selectedSheet: null,
  icons: [],
  sheetBusy: false,
  comparing: null,
  vectorized: {},
  view: "split",
  wipe: 0.5,

  grouping: null,
  preview: null,
  groupBusy: false,
  groupNote: null,
  sensitivity: null,

  sheetPlan: null,
  sheetCsv: null,
  sheetFiles: null,
  sheetGenBusy: false,
  sheetNote: null,

  review: null,
  reviewLog: null,
  reviewOpen: false,
  reviewBusy: false,
  reviewNote: null,
  reviewFilter: "all",
  reviewOrder: "attention",
  reviewSelected: null,
  reviewOverlay: null,
  reviewCrop: null,
  reviewStartedAt: 0,
  reviewExported: null,

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

  async openSheet(sheet) {
    set({
      selectedSheet: sheet,
      icons: [],
      comparing: null,
      sheetBusy: true,
      error: null,
      grouping: null,
      preview: null,
      groupNote: null,
      sensitivity: null,
      sheetPlan: null,
      sheetCsv: null,
      sheetFiles: null,
      sheetNote: null,
      review: null,
      reviewLog: null,
      reviewOpen: false,
      reviewNote: null,
      reviewSelected: null,
      reviewOverlay: null,
      reviewCrop: null,
      reviewExported: null,
      reviewStartedAt: 0,
    });
    try {
      const icons = await backend().then((b) => b.sheetIcons(sheet.id));
      if (get().selectedSheet?.id === sheet.id) set({ icons });
    } catch (e) {
      set({ error: String(e) });
    } finally {
      set({ sheetBusy: false });
    }
  },

  closeSheet() {
    set({
      selectedSheet: null,
      icons: [],
      comparing: null,
      grouping: null,
      preview: null,
      groupNote: null,
      sensitivity: null,
      sheetPlan: null,
      sheetCsv: null,
      sheetFiles: null,
      sheetNote: null,
      review: null,
      reviewLog: null,
      reviewOpen: false,
      reviewNote: null,
      reviewSelected: null,
      reviewOverlay: null,
      reviewCrop: null,
      reviewExported: null,
      reviewStartedAt: 0,
    });
  },

  async reloadIcons() {
    const sheet = get().selectedSheet;
    if (!sheet) return;
    try {
      const icons = await backend().then((b) => b.sheetIcons(sheet.id));
      if (get().selectedSheet?.id === sheet.id) set({ icons });
    } catch (e) {
      set({ error: String(e) });
    }
  },

  async vectorizeSheet() {
    const sheet = get().selectedSheet;
    if (!sheet) return;
    const preset = get().comparing?.preset ?? "flat-8";
    try {
      await backend().then((b) => b.vectorizeSheetSubmit(sheet.id, preset));
    } catch (e) {
      set({ error: String(e) });
    }
  },

  async selectIcon(bbox) {
    const preset = get().comparing?.preset ?? "flat-8";
    set({ comparing: { bbox, preset } });
    await get().ensureIcon(bbox, preset);
  },

  async setPreset(preset) {
    const { comparing } = get();
    if (!comparing || comparing.preset === preset) return;
    set({ comparing: { ...comparing, preset } });
    await get().ensureIcon(comparing.bbox, preset);
  },

  setView(view) {
    set({ view });
  },

  setWipe(wipe) {
    set({ wipe });
  },

  async groupAll() {
    const sheet = get().selectedSheet;
    if (!sheet) return;
    set({ groupBusy: true, error: null, groupNote: null });
    try {
      const b = await backend();
      // The preview fills the mask cache natively, so running it first makes
      // the grouping itself a cache hit and one decode serves both.
      void get().loadPreview();
      const grouping = await b.groupAll(sheet.id);
      if (get().selectedSheet?.id !== sheet.id) return;
      set({ grouping, sensitivity: grouping.sensitivity });
    } catch (e) {
      set({ error: String(e) });
    } finally {
      set({ groupBusy: false });
    }
  },

  async setSensitivity(patch) {
    const sheet = get().selectedSheet;
    const current = get().sensitivity ?? get().grouping?.sensitivity;
    if (!sheet || !current) return;
    const next = clampSensitivity({ ...current, ...patch });
    set({ sensitivity: next, groupBusy: true, error: null });
    try {
      const grouping = await backend().then((b) => b.groupSetSensitivity(sheet.id, next));
      if (get().selectedSheet?.id !== sheet.id) return;
      set({
        grouping,
        sensitivity: grouping.sensitivity,
        groupNote: `re-grouped with the sliders · ${grouping.elapsedMs.toFixed(1)} ms`,
      });
    } catch (e) {
      set({ error: String(e) });
    } finally {
      set({ groupBusy: false });
    }
  },

  async splitHere(x, y) {
    const sheet = get().selectedSheet;
    if (!sheet) return;
    set({ groupBusy: true, error: null });
    try {
      const out = await backend().then((b) => b.groupSplitHere(sheet.id, x, y));
      if (get().selectedSheet?.id !== sheet.id) return;
      set({
        grouping: out.report,
        sensitivity: out.report.sensitivity,
        groupNote: out.split
          ? `split group ${out.groupIndex} into ${out.regions} in ${out.elapsedMs.toFixed(2)} ms`
          : `split refused for group ${out.groupIndex} (no watershed structure)`,
      });
    } catch (e) {
      set({ error: String(e), groupNote: null });
    } finally {
      set({ groupBusy: false });
    }
  },

  async groupSelected(boxes) {
    const sheet = get().selectedSheet;
    if (!sheet || boxes.length === 0) return;
    const editsBefore = get().grouping?.manualEdits ?? 0;
    set({ groupBusy: true, error: null });
    try {
      const grouping = await backend().then((b) => b.groupSelected(sheet.id, boxes));
      if (get().selectedSheet?.id !== sheet.id) return;
      set({
        grouping,
        sensitivity: grouping.sensitivity,
        groupNote:
          grouping.manualEdits > editsBefore
            ? `grouped the marquee · ${grouping.groups.length} icons`
            : "the marquee hit fewer than two groups",
      });
    } catch (e) {
      set({ error: String(e) });
    } finally {
      set({ groupBusy: false });
    }
  },

  async resetGrouping() {
    await get().groupAll();
  },

  async loadPreview() {
    const sheet = get().selectedSheet;
    if (!sheet || get().preview) return;
    try {
      const preview = await backend().then((b) => b.sheetPreview(sheet.id, 1024));
      if (get().selectedSheet?.id === sheet.id) set({ preview });
    } catch (e) {
      set({ error: String(e) });
    }
  },

  clearGrouping() {
    set({
      grouping: null,
      preview: null,
      groupNote: null,
      sensitivity: null,
      sheetPlan: null,
      sheetCsv: null,
      sheetFiles: null,
      sheetNote: null,
    });
  },

  async ensureIcon(bbox, preset) {
    const sheet = get().selectedSheet;
    if (!sheet) return;
    const key = iconKey(bbox, preset);
    if (get().vectorized[key]) return;
    try {
      const svg = await backend().then((b) =>
        b.vectorizeIcon(sheet.id, bbox[0], bbox[1], bbox[2], bbox[3], preset),
      );
      set({ vectorized: { ...get().vectorized, [key]: svg } });
    } catch (e) {
      set({ error: String(e) });
    }
  },

  async planSheet(request) {
    const sheet = get().selectedSheet;
    if (!sheet) return;
    set({ sheetGenBusy: true, error: null, sheetNote: null });
    try {
      const plan = await backend().then((b) => b.sheetPlan({ ...request, sheetId: sheet.id }));
      if (get().selectedSheet?.id !== sheet.id) return;
      set({
        sheetPlan: plan,
        sheetNote: `${plan.icons} icons planned · ${plan.columns}×${plan.rows} cells`,
      });
    } catch (e) {
      set({ error: String(e) });
    } finally {
      set({ sheetGenBusy: false });
    }
  },

  async previewSheetCsv(request, csv) {
    const sheet = get().selectedSheet;
    if (!sheet) return;
    set({ sheetGenBusy: true, error: null });
    try {
      const preview = await backend().then((b) =>
        b.sheetCsvPreview({ ...request, sheetId: sheet.id }, csv),
      );
      if (get().selectedSheet?.id !== sheet.id) return;
      set({
        sheetCsv: preview,
        sheetNote: `${preview.rows.length} rows · ${preview.columns.length} columns`,
      });
    } catch (e) {
      set({ error: String(e) });
    } finally {
      set({ sheetGenBusy: false });
    }
  },

  async exportSheet(request) {
    const sheet = get().selectedSheet;
    if (!sheet) return;
    set({ sheetGenBusy: true, error: null, sheetNote: null });
    try {
      const out = await backend().then((b) => b.sheetExport({ ...request, sheetId: sheet.id }));
      if (get().selectedSheet?.id !== sheet.id) return;
      set({
        sheetPlan: out.plan,
        sheetFiles: out.files,
        sheetNote: `wrote ${out.files.length} file(s) into ${request.outDir}`,
      });
    } catch (e) {
      set({ error: String(e) });
    } finally {
      set({ sheetGenBusy: false });
    }
  },

  clearSheetPlan() {
    set({ sheetPlan: null, sheetCsv: null, sheetFiles: null, sheetNote: null });
  },

  // ---- Review workspace (§3.6) -------------------------------------------

  async openReview(force) {
    const sheet = get().selectedSheet;
    if (!sheet) return;
    const cached = get().review;
    if (!force && cached && get().reviewOpen === false && cached.sheet === sheet.id) {
      // Reopening a workspace whose pass is still cached must not cost the
      // 17 s a real 1000-icon pass takes.
      set({ reviewOpen: true, reviewNote: null });
      return;
    }
    set({ reviewBusy: true, reviewOpen: true, error: null, reviewNote: null });
    try {
      const review = await backend().then((b) => b.reviewRun(sheet.id));
      if (get().selectedSheet?.id !== sheet.id) return;
      const rows = orderRows(review.icons, get().reviewOrder);
      set({
        review,
        reviewLog: review.triage,
        reviewFilter: "all",
        reviewSelected: rows.length > 0 ? rows[0].id : null,
        reviewOverlay: null,
        reviewCrop: null,
        reviewExported: null,
        reviewStartedAt: Date.now(),
        reviewNote:
          `${review.icons.length} icons reviewed · ${review.flagged} flagged · ` +
          `${review.clusters.length} clusters · ${review.triage.decided} already decided`,
      });
    } catch (e) {
      set({ error: String(e), reviewOpen: false });
    } finally {
      set({ reviewBusy: false });
    }
  },

  closeReview() {
    set({ reviewOpen: false, reviewOverlay: null, reviewCrop: null, reviewNote: null });
  },

  setReviewFilter(filter) {
    const review = get().review;
    set({ reviewFilter: filter });
    if (!review) return;
    const rows = filterRows(orderRows(review.icons, get().reviewOrder), filter);
    const current = get().reviewSelected;
    set({
      reviewSelected: rows.some((row) => row.id === current) ? current : rows[0]?.id ?? null,
    });
  },

  setReviewOrder(order) {
    const review = get().review;
    set({ reviewOrder: order });
    if (!review) return;
    const rows = orderRows(review.icons, order);
    const current = get().reviewSelected;
    const visible = filterRows(rows, get().reviewFilter);
    set({
      reviewSelected: visible.some((row) => row.id === current)
        ? current
        : visible[0]?.id ?? null,
    });
  },

  moveReviewSelection(delta, skipDecided = true) {
    const review = get().review;
    if (!review) return;
    const rows = filterRows(orderRows(review.icons, get().reviewOrder), get().reviewFilter);
    set({ reviewSelected: nextRow(rows, get().reviewSelected, delta, skipDecided) });
  },

  selectReviewIcon(iconId) {
    set({ reviewSelected: iconId });
  },

  async decideReview(iconId, action) {
    const sheet = get().selectedSheet;
    if (!sheet) return;
    set({ reviewBusy: true, error: null });
    try {
      // The command needs the sheet and the icon id, not a rendered report: a
      // decision is a journal row, so it must not depend on which screen is up.
      const triage = await backend().then((b) => b.reviewApply(sheet.id, iconId, action));
      const live = get().review;
      if (get().selectedSheet?.id !== sheet.id) return;
      if (!live) {
        set({ reviewLog: triage, reviewNote: `${action} · ${triage.decided} decided` });
        return;
      }
      // The log's state is the backend's answer; the row's chip is the action's
      // own name, which is what `state_name` reads back off the log. Sequence
      // numbers and `csvBytes` are never guessed here.
      const patched = patchState({ ...live, triage }, iconId, stateAfter(action));
      const rows = filterRows(orderRows(patched.icons, get().reviewOrder), get().reviewFilter);
      set({
        review: patched,
        reviewLog: triage,
        reviewSelected: nextRow(rows, iconId, 1),
        reviewNote: `${action} · ${triage.decided} of ${patched.icons.length} decided`,
      });
    } catch (e) {
      set({ error: String(e) });
    } finally {
      set({ reviewBusy: false });
    }
  },

  async bulkApproveReview() {
    const sheet = get().selectedSheet;
    const review = get().review;
    if (!sheet || !review) return;
    const pending = review.icons.filter((icon) => icon.state === "pending");
    if (pending.length === 0) {
      set({ reviewNote: "nothing left undecided" });
      return;
    }
    set({ reviewBusy: true, error: null });
    try {
      const b = await backend();
      let triage = review.triage;
      let done = 0;
      for (const icon of pending) {
        // One command per icon: the log's sequence numbers are its own, so
        // batching them in the frontend would invent an order the audit trail
        // then has to live with.
        triage = await b.reviewApply(sheet.id, icon.id, "bulk-approve");
        done += 1;
        const live = get().review;
        if (get().selectedSheet?.id !== sheet.id) return;
        if (!live) return;
        set({
          review: patchState({ ...live, triage }, icon.id, stateAfter("bulk-approve")),
          reviewNote: `bulk approve · ${done} of ${pending.length} · ${triage.decided} decided`,
        });
      }
      const live = get().review;
      if (live && get().selectedSheet?.id === sheet.id) {
        const rows = filterRows(orderRows(live.icons, get().reviewOrder), get().reviewFilter);
        set({
          reviewSelected: rows.some((row) => row.id === get().reviewSelected)
            ? get().reviewSelected
            : rows[0]?.id ?? null,
          reviewNote: `approved ${done} icon(s) in bulk · ${triage.decided} decided`,
        });
      }
    } catch (e) {
      set({ error: String(e) });
    } finally {
      set({ reviewBusy: false });
    }
  },

  async undoReview() {
    const sheet = get().selectedSheet;
    if (!sheet) return;
    set({ reviewBusy: true, error: null });
    try {
      const undone = await backend().then((b) => b.reviewUndo(sheet.id));
      if (get().selectedSheet?.id !== sheet.id) return;
      if (!undone) {
        set({ reviewNote: "nothing left to undo" });
        return;
      }
      const live = get().review;
      if (!live) {
        set({
          reviewLog: undone.triage,
          reviewNote: `undid the last decision · ${undone.restored}`,
        });
        return;
      }
      // The cursor goes back to the row that changed: the reviewer undid it to
      // look at it again, not to keep walking.
      const patched = patchState({ ...live, triage: undone.triage }, undone.icon, undone.restored);
      set({
        review: patched,
        reviewLog: undone.triage,
        reviewSelected: undone.icon,
        reviewNote: `undid the last decision · ${undone.restored} · ${undone.triage.decided} decided`,
      });
    } catch (e) {
      set({ error: String(e) });
    } finally {
      set({ reviewBusy: false });
    }
  },

  async toggleReviewOverlay(iconId) {
    const sheet = get().selectedSheet;
    if (!sheet || !get().review) return;
    // `Space` (§3.6) dismisses whatever is pinned; a click on a row's overlay
    // button names its own icon and switches to it.
    if (iconId === undefined && get().reviewOverlay !== null) {
      set({ reviewOverlay: null, reviewCrop: null });
      return;
    }
    const id = iconId ?? get().reviewSelected;
    if (!id) return;
    if (get().reviewOverlay === id) {
      set({ reviewOverlay: null, reviewCrop: null });
      return;
    }
    // The overlay is the icon's own pixels from the sheet. The review report has
    // no bbox — it measures the traced document — but the library row does, and
    // the two share the icon id, which is why the join is by id and not by index.
    const tile = get().icons.find((icon) => icon.id === id);
    const row = get().review?.icons.find((icon) => icon.id === id);
    set({ reviewOverlay: id, reviewCrop: null });
    if (!tile) {
      set({ reviewNote: `no sheet row for ${id.slice(0, 6)}… — the icon list is empty or stale` });
      return;
    }
    try {
      const crop = await backend().then((b) =>
        b.sheetCrop(sheet.id, tile.bbox[0], tile.bbox[1], tile.bbox[2], tile.bbox[3]),
      );
      if (get().reviewOverlay !== id) return;
      set({
        reviewCrop: crop,
        reviewNote: row
          ? `row ${row.index} · ${row.nodeCount} nodes · ${row.stat.stroke.toFixed(1)} px stroke · ` +
            `${row.closed ? "closed" : "open"} outline`
          : null,
      });
    } catch (e) {
      set({ error: String(e) });
    }
  },

  async refreshReviewState() {
    const sheet = get().selectedSheet;
    if (!sheet) return;
    try {
      // Cheap on purpose: `review_state` replays the journal, it does not run
      // the detectors — so the drawer can say what a sheet's log already holds
      // before anyone pays for a pass.
      const triage = await backend().then((b) => b.reviewState(sheet.id));
      if (get().selectedSheet?.id !== sheet.id) return;
      const live = get().review;
      set({ reviewLog: triage, ...(live ? { review: { ...live, triage } } : {}) });
    } catch (e) {
      set({ error: String(e) });
    }
  },

  async exportReview(outDir) {
    const sheet = get().selectedSheet;
    if (!sheet || !get().review) return;
    set({ reviewBusy: true, error: null });
    try {
      const out = await backend().then((b) =>
        b.reviewExport(outDir ? { sheetId: sheet.id, outDir } : { sheetId: sheet.id }),
      );
      if (get().selectedSheet?.id !== sheet.id) return;
      set({
        reviewExported: out,
        reviewNote: out.path
          ? `wrote ${out.path} · ${out.decisions} decisions`
          : `preview · ${out.decisions} decisions · ${formatBytes(out.csv.length)}`,
      });
    } catch (e) {
      set({ error: String(e) });
    } finally {
      set({ reviewBusy: false });
    }
  },

  clearReview() {
    set({
      review: null,
      reviewLog: null,
      reviewOpen: false,
      reviewBusy: false,
      reviewNote: null,
      reviewFilter: "all",
      reviewSelected: null,
      reviewOverlay: null,
      reviewCrop: null,
      reviewExported: null,
      reviewStartedAt: 0,
    });
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
        // When an import finishes, refresh the visible page. Vectorize jobs
        // also refresh the open sheet's icon list (rows land in the DB).
        if (e.kind === "finished" && e.outcome === "succeeded") {
          void get().refreshPage(0, 200);
          if (jobs[e.id]?.name.startsWith("Vectorize")) {
            void get().reloadIcons();
          }
        }
      });
    })();
  },
}));
