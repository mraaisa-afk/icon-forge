/**
 * Editor state: one loaded document, its selection, and the history the toolbar
 * drives.
 *
 * All editing happens in the WASM session (ARCHITECTURE §2: per-frame work never
 * crosses the IPC boundary). The store's job is to keep the React-visible mirror
 * of that state in sync and to translate UI gestures into ABI commands — nothing
 * here computes geometry.
 *
 * Loading an icon *does* cross the boundary, once per icon: the Rust tracer
 * produces the outlines (Phase 2/3 work, cached by stage-8 keys) and the adapter
 * turns them into node geometry. 4A caps how many icons it loads (see
 * [`EDITOR_ICON_LIMIT`]) so opening the editor on a 1024-icon sheet stays snappy;
 * paging the rest in is 4B work.
 */

import { create } from "zustand";

import type {
  AlignEdge,
  AlignFrame,
  ArrangeTo,
  BooleanOp,
  Box,
  EditorCommand,
  NodeSpec,
  PointAddress,
  Rgba,
  SegmentAddress,
  SnapGuide,
  VertexAddress,
} from "../wasm/abi";
import { EditorSession, type HistoryView } from "../wasm/editor";
import {
  SNAP_DEFAULTS,
  snapFlags,
  type Affine,
  type SnapSettings,
} from "../components/editor/canvasModel";
import { backend, type Box4, type SheetDto } from "../lib/backend";

/** How many icons one editor session loads (the rest is 4B work). */
export const EDITOR_ICON_LIMIT = 64;

/** Where the app serves the built editor module from (`public/`). */
export const EDITOR_MODULE_URL = "isg_wasm.wasm";

/** The preset icons are traced with when the editor loads them. */
export const EDITOR_PRESET = "flat-8";

/** Fallback fill for a traced icon whose SVG carries no usable colour. */
const DEFAULT_FILL: Rgba = [214, 219, 227, 255];

/** Colours the fill button cycles through. */
export const FILL_SWATCHES: readonly Rgba[] = [
  [214, 219, 227, 255],
  [56, 132, 255, 255],
  [244, 114, 182, 255],
  [52, 211, 153, 255],
  [251, 191, 36, 255],
  [248, 113, 113, 255],
  [167, 139, 250, 255],
];

export type EditorStatus = "closed" | "loading" | "ready" | "unavailable";

const EMPTY_HISTORY: HistoryView = {
  canUndo: false,
  canRedo: false,
  undoable: 0,
  redoable: 0,
  dropped: 0,
  undoLabel: null,
  redoLabel: null,
  lastLabel: null,
};

export interface EditorState {
  status: EditorStatus;
  /** Why the editor is unavailable (module missing, backend refused, …). */
  error: string | null;
  /** What just happened, shown under the canvas. */
  note: string | null;
  sheetId: string | null;
  /** Canvas size in document units (sheet pixels). */
  width: number;
  height: number;
  /** Every node, bottom first — the canvas draws these. */
  nodes: NodeSpec[];
  selection: number[];
  history: HistoryView;
  revision: number;
  /** How many icons were loaded versus how many the sheet has. */
  loadedIcons: number;
  availableIcons: number;
  /** The selection's box, as the engine reports it (previews included). */
  selectionBox: Box | null;
  /** What snapping is allowed to use, and how close counts. */
  snap: SnapSettings;
  /** Guides the last snap answered with — the overlay draws these. */
  guides: SnapGuide[];
  /** The command a live gesture is previewing, if any. */
  previewing: EditorCommand | null;

  /** Loads the module (once) and the listed icons as an editable document. */
  open: (sheet: SheetDto, boxes: readonly Box4[]) => Promise<void>;
  /** Installs an already-created session (the seam the tests use). */
  adopt: (
    session: EditorSession,
    width: number,
    height: number,
    nodes: NodeSpec[],
    sheetId?: string,
  ) => void;
  close: () => void;
  /** Re-reads nodes, selection and history from the session. */
  sync: () => void;
  apply: (command: EditorCommand) => void;
  undo: () => void;
  redo: () => void;
  /** A click at a document point; `additive` toggles instead of replacing. */
  click: (x: number, y: number, additive: boolean) => void;
  /** A completed marquee; selects everything it touched. */
  marquee: (box: Box) => void;
  selectAll: () => void;
  clearSelection: () => void;
  /** Cycles the next fill swatch onto the selection. */
  cycleFill: () => void;
  toggleVisible: () => void;
  duplicate: () => void;
  remove: () => void;
  nudge: (dx: number, dy: number) => void;

  // -- 4B: transforms, groups, snapping --------------------------------------
  /** Changes one snapping setting (the grid step and tolerance included). */
  setSnap: (patch: Partial<SnapSettings>) => void;
  /** Asks where a proposed move lands, and records the guides to draw. */
  snapMove: (dx: number, dy: number) => { dx: number; dy: number; guides: SnapGuide[] };
  /** Shows a transform the engine has not recorded (a gesture in progress). */
  preview: (command: EditorCommand) => void;
  /** Commits the command the preview showed, as exactly one history step. */
  commit: (command: EditorCommand) => void;
  /** Drops the preview and its guides (a cancelled gesture). */
  cancelPreview: () => void;
  /** Replaces the guides the overlay draws (the canvas clears them). */
  setGuides: (guides: SnapGuide[]) => void;
  align: (frame: AlignFrame, edge: AlignEdge) => void;
  arrange: (to: ArrangeTo) => void;
  group: () => void;
  ungroup: () => void;
  /** Resizes the selection to a width and/or height in document units. */
  resize: (size: { width?: number; height?: number }) => void;
  /** Turns the selection about its centre. */
  rotate: (degrees: number) => void;

  // -- 4C: node editing, booleans, SVG import --------------------------------
  /** Combines the selection into one node with a pathfinder operation. */
  booleanOp: (op: BooleanOp) => void;
  /** Imports SVG text into the open document, one node per path. */
  importSvg: (text: string) => void;
  /** One 4C point edit; the address identifies the point inside `node`. */
  editPoint: (node: number, at: PointAddress, to: { x: number; y: number } | null) => void;
  /** Converts a segment between a line and a cubic. */
  setSegment: (node: number, at: SegmentAddress, to: "line" | "cubic") => void;
  /** Deletes a vertex, joining its neighbours. */
  deletePoint: (node: number, at: VertexAddress) => void;
}

/** The live session, outside React state: it is not serialisable and never rendered. */
let session: EditorSession | null = null;

/** The session the store is driving (null before `open`/`adopt`). */
export function editorSession(): EditorSession | null {
  return session;
}

/** The identity transform, as the ABI's `m`. */
const IDENTITY: Affine = [1, 0, 0, 1, 0, 0];

/**
 * Turns an SVG into nodes, one per `<path>` in the file, numbered from `firstId`.
 *
 * The *engine* parses the file (4A's TypeScript parser is retired — see
 * `ARCHITECTURE.md` §3.9), so a traced outline is read by exactly the code that
 * draws it, and each shape keeps its own colour and its own transform (which
 * rides on the node's `m`, exactly as for any other node). A file the engine
 * refuses, or one that draws nothing, yields no nodes rather than an empty icon.
 *
 * A traced icon arrives as one path per colour layer, so callers that think in
 * icons group the result (see `open`) rather than merging the geometry: layers
 * overlap, and cramming them into one node would fill their intersections as
 * holes.
 */
export function nodesFromSvg(session: EditorSession, firstId: number, svg: string): NodeSpec[] {
  const parsed = session.svgNodes(svg, firstId, IDENTITY, DEFAULT_FILL);
  return parsed.ok ? parsed.value : [];
}

export const useEditor = create<EditorState>((set, get) => {
  const sync = (): void => {
    if (!session) {
      set({ nodes: [], selection: [], selectionBox: null, guides: [], history: EMPTY_HISTORY });
      return;
    }
    set({
      nodes: session.nodes(),
      selection: session.selection(),
      selectionBox: session.selectionBounds(),
      history: session.history(),
      revision: session.revision,
    });
  };

  /** Applies one command and reports what happened in the status line. */
  const edit = (command: EditorCommand, describe: string): void => {
    if (!session) return;
    const result = session.apply(command);
    set({ note: result.ok ? `${describe} · ${result.value} node${result.value === 1 ? "" : "s"}` : result.message });
    sync();
  };

  const step = (direction: "undo" | "redo"): void => {
    if (!session) return;
    const result = direction === "undo" ? session.undo() : session.redo();
    set({
      note: result.ok
        ? `${direction === "undo" ? "undid" : "redid"} ${result.value}`
        : result.message,
    });
    sync();
  };

  return {
    status: "closed",
    error: null,
    note: null,
    sheetId: null,
    width: 0,
    height: 0,
    nodes: [],
    selection: [],
    history: EMPTY_HISTORY,
    revision: 0,
    loadedIcons: 0,
    availableIcons: 0,
    selectionBox: null,
    snap: SNAP_DEFAULTS,
    guides: [],
    previewing: null,

    async open(sheet, boxes) {
      set({
        status: "loading",
        error: null,
        note: "loading the editor module…",
        sheetId: sheet.id,
        width: sheet.width,
        height: sheet.height,
      });
      try {
        if (!session || !session.compatible) {
          const url =
            typeof document === "undefined"
              ? EDITOR_MODULE_URL
              : new URL(EDITOR_MODULE_URL, document.baseURI).href;
          session = await EditorSession.fromUrl(url);
        }
        const wanted = boxes.slice(0, EDITOR_ICON_LIMIT);
        const api = await backend();
        const nodes: NodeSpec[] = [];
        let icons = 0;
        for (const box of wanted) {
          const [x, y, w, h] = box;
          const { svg } = await api.vectorizeIcon(sheet.id, x, y, w, h, EDITOR_PRESET);
          const parsed = nodesFromSvg(session, nodes.length + 1, svg);
          if (parsed.length === 0) continue;
          icons += 1;
          // An icon is one thing to move, however many layers it is drawn in:
          // a multi-path icon is loaded as a group, so a click selects the whole
          // icon and ungrouping hands the layers back for editing.
          if (parsed.length > 1) for (const node of parsed) node.group = icons;
          nodes.push(...parsed);
        }
        if (nodes.length === 0) {
          set({
            status: "unavailable",
            error: "none of the selected icons produced an outline",
            note: null,
          });
          return;
        }
        const loaded = session.loadDocument(sheet.width, sheet.height, nodes);
        if (!loaded.ok) {
          set({ status: "unavailable", error: loaded.message, note: null });
          return;
        }
        set({
          status: "ready",
          error: null,
          note: `${icons} icon${icons === 1 ? "" : "s"} ready (${nodes.length} node${nodes.length === 1 ? "" : "s"}) · drag to move, drag empty space to select`,
          loadedIcons: icons,
          availableIcons: boxes.length,
        });
        sync();
      } catch (cause) {
        session = null;
        set({
          status: "unavailable",
          error:
            cause instanceof Error ? cause.message : "the editor module could not be loaded",
          note: null,
        });
      }
    },

    adopt(adopted, width, height, nodes, sheetId: string | null = null) {
      session = adopted;
      set({
        status: "ready",
        error: null,
        note: null,
        sheetId,
        width,
        height,
        loadedIcons: nodes.length,
        availableIcons: nodes.length,
      });
      sync();
    },

    close() {
      session?.close();
      session = null;
      set({
        status: "closed",
        note: null,
        error: null,
        nodes: [],
        selection: [],
        history: EMPTY_HISTORY,
        loadedIcons: 0,
        availableIcons: 0,
        sheetId: null,
        selectionBox: null,
        guides: [],
        previewing: null,
      });
    },

    sync,
    apply: (command) => edit(command, command.kind),
    undo: () => step("undo"),
    redo: () => step("redo"),

    click(x, y, additive) {
      if (!session) return;
      const id = session.pick(x, y);
      if (id === 0) {
        if (!additive) session.clearSelection();
        set({ note: "nothing there" });
      } else if (additive) {
        session.selectToggle(id);
        set({ note: `node ${id} toggled` });
      } else {
        session.selectOnly(id);
        set({ note: `node ${id} selected` });
      }
      sync();
    },

    marquee(box) {
      if (!session) return;
      const ids = session.marquee(box);
      if (ids.length === 0) {
        session.clearSelection();
        set({ note: "the marquee caught nothing" });
      } else {
        session.selectOnly(ids[0]);
        for (const id of ids.slice(1)) session.selectAdd(id);
        set({ note: `${ids.length} node${ids.length === 1 ? "" : "s"} selected` });
      }
      sync();
    },

    selectAll() {
      if (!session) return;
      const count = session.selectAll();
      set({ note: `${count} node${count === 1 ? "" : "s"} selected` });
      sync();
    },

    clearSelection() {
      session?.clearSelection();
      sync();
    },

    cycleFill() {
      if (!session) return;
      const { selection, nodes } = get();
      if (selection.length === 0) {
        set({ note: "select something first" });
        return;
      }
      const current = nodes.find((node) => node.id === selection[0])?.fill;
      const index = FILL_SWATCHES.findIndex(
        (swatch) => current !== undefined && swatch.every((channel, i) => channel === current[i]),
      );
      const next = FILL_SWATCHES[(index + 1) % FILL_SWATCHES.length];
      edit({ kind: "fill", rgba: next }, "fill");
    },

    toggleVisible() {
      if (!session) return;
      const { selection, nodes } = get();
      if (selection.length === 0) {
        set({ note: "select something first" });
        return;
      }
      const visible = nodes.find((node) => node.id === selection[0])?.visible ?? true;
      edit({ kind: "visible", to: !visible }, visible ? "hide" : "show");
    },

    duplicate() {
      edit({ kind: "duplicate", dx: 8, dy: 8 }, "duplicate");
    },

    remove() {
      edit({ kind: "delete" }, "delete");
    },

    nudge(dx, dy) {
      edit({ kind: "translate", dx, dy }, "move");
    },

    // -- 4B: transforms, groups, snapping ------------------------------------

    setSnap(patch) {
      set({ snap: { ...get().snap, ...patch } });
    },

    snapMove(dx, dy) {
      if (!session) return { dx, dy, guides: [] };
      const flags = snapFlags(get().snap);
      if (flags === 0) return { dx, dy, guides: [] };
      const answer = session.snapMove(dx, dy, {
        tolerance: get().snap.tolerance,
        flags,
        gridStep: get().snap.gridStep,
      });
      if (!answer.ok) {
        // Nothing to snap to (or a refused request): the raw delta stands.
        set({ note: answer.message, guides: [] });
        return { dx, dy, guides: [] };
      }
      set({ guides: answer.value.guides });
      return answer.value;
    },

    preview(command) {
      if (!session) return;
      const result = session.preview(command);
      // A gesture in progress must not spam the status line, but a refusal has
      // to say why nothing is moving.
      set(result.ok ? { previewing: command } : { previewing: null, note: result.message });
    },

    commit(command) {
      edit(command, command.kind);
      set({ previewing: null, guides: [] });
    },

    cancelPreview() {
      session?.clearPreview();
      set({ previewing: null, guides: [] });
    },

    setGuides(guides) {
      set({ guides });
    },

    align(frame, edge) {
      edit({ kind: "align", frame, edge }, `align ${edge}`);
    },

    arrange(to) {
      edit({ kind: "arrange", to }, `bring to ${to}`);
    },

    group() {
      edit({ kind: "group" }, "group");
    },

    ungroup() {
      edit({ kind: "ungroup" }, "ungroup");
    },

    resize(size) {
      if (!session) return;
      const box = session.selectionBounds();
      if (!box) {
        set({ note: "select something first" });
        return;
      }
      const width = size.width ?? box.x1 - box.x0;
      const height = size.height ?? box.y1 - box.y0;
      const spanX = box.x1 - box.x0;
      const spanY = box.y1 - box.y0;
      if (spanX <= 0 || spanY <= 0 || width <= 0 || height <= 0) {
        set({ note: "a selection needs a positive size to resize from" });
        return;
      }
      edit(
        { kind: "scaleXY", sx: width / spanX, sy: height / spanY, pivot: { x: box.x0, y: box.y0 } },
        "resize",
      );
    },

    rotate(degrees) {
      if (!session) return;
      const box = session.selectionBounds();
      if (!box) {
        set({ note: "select something first" });
        return;
      }
      const pivot = { x: (box.x0 + box.x1) / 2, y: (box.y0 + box.y1) / 2 };
      edit({ kind: "rotate", degrees, pivot }, "rotate");
    },

    // -- 4C: node editing, booleans, SVG import ------------------------------

    booleanOp(op) {
      if (!session) return;
      if (get().selection.length < 2) {
        set({ note: "a boolean needs two or more shapes" });
        return;
      }
      edit({ kind: "boolean", op }, op);
    },

    importSvg(text) {
      if (!session) return;
      const result = session.importSvg(text, IDENTITY, DEFAULT_FILL);
      set({
        note: result.ok
          ? `imported ${result.value} shape${result.value === 1 ? "" : "s"}`
          : result.message,
      });
      sync();
    },

    editPoint(node, at, to) {
      if (!session) return;
      if (at.of === "vertex") {
        // With a target the vertex moves; without one the gesture is a delete.
        if (to) edit({ kind: "movePoint", node, at, to }, "point");
        else edit({ kind: "deletePoint", node, at }, "delete point");
        return;
      }
      if (at.of === "handle") {
        if (to) edit({ kind: "moveHandle", node, at, to }, "handle");
        return;
      }
      edit({ kind: "insertPoint", node, at }, "insert point");
    },

    setSegment(node, at, to) {
      edit({ kind: "setSegment", node, at, to }, `segment ${to}`);
    },

    deletePoint(node, at) {
      edit({ kind: "deletePoint", node, at }, "delete point");
    },
  };
});
