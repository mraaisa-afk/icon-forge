/**
 * Editor store tests.
 *
 * The mirror tests run against the real WebAssembly artifact (see
 * `src/wasm/editor.test.ts` for how to build it) because the store's whole job
 * is to keep React in step with the engine — a fake session would test the fake.
 * The always-on part covers the pure conversion and the failure paths.
 */

import { afterEach, describe, expect, it } from "vitest";

import { EditorSession } from "../wasm/editor";
import { KIND, SNAP, type NodeSpec, type Subpath } from "../wasm/abi";
import { artifactAvailable as available, loadArtifact } from "../wasm/artifact";
import { editorSession, useEditor, nodeFromSvg, EDITOR_ICON_LIMIT } from "./editorStore";
import { SNAP_DEFAULTS } from "../components/editor/canvasModel";

function square(x: number, y: number, size: number): Subpath {
  return {
    start: { x, y },
    closed: true,
    segs: [
      { kind: KIND.LINE, to: { x: x + size, y } },
      { kind: KIND.LINE, to: { x: x + size, y: y + size } },
      { kind: KIND.LINE, to: { x, y: y + size } },
    ],
  };
}

async function adoptFixture(): Promise<void> {
  const bytes = loadArtifact();
  if (!bytes) throw new Error("the editor artifact is missing");
  const session = await EditorSession.fromBytes(bytes);
  const nodes: NodeSpec[] = [
    { id: 1, m: [1, 0, 0, 1, 0, 0], fill: [255, 0, 0, 255], visible: true, path: [square(10, 10, 20)] },
    { id: 2, m: [1, 0, 0, 1, 60, 10], fill: [0, 255, 0, 255], visible: true, path: [square(0, 0, 20)] },
  ];
  const loaded = session.loadDocument(200, 120, nodes);
  expect(loaded.ok).toBe(true);
  useEditor.getState().adopt(session, 200, 120, nodes, "sheet-1");
}

afterEach(() => {
  useEditor.getState().close();
});

describe("nodeFromSvg", () => {
  const SVG = `<svg><path d="M0 0 L10 0 L10 10 Z" fill="#3366ff"/><path d="M20 20 L30 20 L30 30 Z" fill="#ff0000"/></svg>`;

  it("turns a traced icon into one node carrying every subpath", () => {
    const node = nodeFromSvg(7, SVG);
    expect(node?.id).toBe(7);
    expect(node?.fill).toEqual([0x33, 0x66, 0xff, 255]);
    expect(node?.visible).toBe(true);
    expect(node?.path).toHaveLength(2);
    expect(node?.m).toEqual([1, 0, 0, 1, 0, 0]);
  });

  it("falls back to a readable fill and to null for an empty trace", () => {
    const noFill = nodeFromSvg(1, `<svg><path d="M0 0 L1 1"/></svg>`);
    expect(noFill?.fill).toEqual([214, 219, 227, 255]);
    expect(nodeFromSvg(1, `<svg><rect width="1" height="1"/></svg>`)).toBeNull();
  });
});

describe("editor store without a session", () => {
  it("starts closed", () => {
    expect(useEditor.getState().status).toBe("closed");
    expect(useEditor.getState().nodes).toEqual([]);
  });

  it("ignores commands that need a document", () => {
    const before = useEditor.getState();
    before.undo();
    before.redo();
    before.click(1, 1, false);
    before.selectAll();
    before.cycleFill();
    before.remove();
    expect(useEditor.getState().status).toBe("closed");
    expect(useEditor.getState().nodes).toEqual([]);
  });

  it("reports a module it cannot load instead of throwing", async () => {
    await useEditor.getState().open(
      {
        id: "sheet-1",
        sourcePath: "/tmp/sheet.png",
        contentHash: "0".repeat(32),
        width: 100,
        height: 100,
        importedAt: "2026-09-18T00:00:00Z",
      },
      [[0, 0, 10, 10]],
    );
    expect(useEditor.getState().status).toBe("unavailable");
    expect(useEditor.getState().error).not.toBeNull();
  });

  it("says how many icons 4A will load", () => {
    expect(EDITOR_ICON_LIMIT).toBeGreaterThan(0);
  });
});

describe.skipIf(!available)("editor store mirroring the engine", () => {
  it("mirrors selection and edits, and keeps one undo step per command", async () => {
    await adoptFixture();
    let state = useEditor.getState();
    expect(state.status).toBe("ready");
    expect(state.nodes).toHaveLength(2);
    expect(state.loadedIcons).toBe(2);

    state.selectAll();
    state = useEditor.getState();
    expect(state.selection).toEqual([1, 2]);

    state.nudge(3, 4);
    state = useEditor.getState();
    expect(state.history.undoable).toBe(1);
    expect(state.history.undoLabel).toBe("move");
    expect(state.note).toMatch(/move · 2 nodes/);
    expect(state.nodes[0].m.slice(4)).toEqual([3, 4]);

    state.undo();
    state = useEditor.getState();
    expect(state.nodes[0].m).toEqual([1, 0, 0, 1, 0, 0]);
    expect(state.history.canRedo).toBe(true);
    expect(state.note).toMatch(/undid move/);

    state.redo();
    state = useEditor.getState();
    expect(state.nodes[0].m.slice(4)).toEqual([3, 4]);
  });

  it("selects what a click lands on and fills it", async () => {
    await adoptFixture();
    useEditor.getState().click(15, 15, false);
    let state = useEditor.getState();
    expect(state.selection).toEqual([1]);
    expect(state.note).toMatch(/node 1 selected/);

    state.cycleFill();
    state = useEditor.getState();
    expect(state.nodes[0].fill).not.toEqual([255, 0, 0, 255]);
    expect(state.history.undoLabel).toBe("fill");

    state.toggleVisible();
    state = useEditor.getState();
    expect(state.nodes[0].visible).toBe(false);
    expect(state.note).toMatch(/hide/);
  });

  it("selects a marquee's contents", async () => {
    await adoptFixture();
    useEditor.getState().marquee({ x0: 0, y0: 0, x1: 40, y1: 40 });
    const state = useEditor.getState();
    expect(state.selection).toEqual([1]);
    expect(state.note).toMatch(/1 node selected/);
  });

  it("reports a refusal without touching the document", async () => {
    await adoptFixture();
    useEditor.getState().click(190, 110, false); // empty space
    const state = useEditor.getState();
    expect(state.selection).toEqual([]);
    expect(state.note).toMatch(/nothing there/);
    state.remove();
    expect(useEditor.getState().note).toMatch(/select something first/i);
    expect(useEditor.getState().nodes).toHaveLength(2);
  });

  it("mirrors the visible/hidden split for a click on a hidden node", async () => {
    await adoptFixture();
    useEditor.getState().click(15, 15, false);
    useEditor.getState().toggleVisible();
    // The node is still selectable (its geometry is still in the document), but
    // it no longer answers a pick: hidden nodes are skipped by hit testing.
    useEditor.getState().click(15, 15, false);
    expect(useEditor.getState().selection).toEqual([]);
  });
});

describe.skipIf(!available)("editor store 4B surface", () => {
  it("snaps a proposed move, draws the guides, and passes through when off", async () => {
    await adoptFixture();
    const state = useEditor.getState();
    // Node 1 sits at x 10..30, y 10..30; the canvas is 200x120.
    state.click(15, 15, false);
    expect(useEditor.getState().selectionBox).toEqual({ x0: 10, y0: 10, x1: 30, y1: 30 });

    // Canvas snapping pulls a nearly-aligned nudge onto the canvas edge (x = 0).
    useEditor.getState().setSnap({ nodes: false, grid: false, canvas: true, tolerance: 3 });
    const snapped = useEditor.getState().snapMove(-8.5, 0);
    expect(snapped.dx).toBe(-10);
    expect(snapped.guides.length).toBeGreaterThan(0);
    expect(useEditor.getState().guides.length).toBe(snapped.guides.length);

    // With every family switched off the delta comes back untouched and no
    // guide is remembered from the previous answer.
    useEditor.getState().setSnap({ canvas: false, nodes: false, grid: false });
    const raw = useEditor.getState().snapMove(-8.5, 2.25);
    expect(raw.dx).toBe(-8.5);
    expect(raw.dy).toBe(2.25);
    expect(raw.guides).toEqual([]);

    // The grid is a family of its own, with its own step: the box's left edge
    // (10 + 7 = 17) is one unit from the 16 line, so the delta loses that unit.
    useEditor.getState().setSnap({ grid: true, gridStep: 8, tolerance: 6 });
    const grid = useEditor.getState().snapMove(7, 0);
    expect(grid.dx).toBe(6);
    expect(grid.guides.length).toBeGreaterThan(0);
    expect(grid.guides.every((guide) => guide.kind === 4)).toBe(true);
    expect(grid.guides.every((guide) => Math.abs(guide.position % 8) < 1e-4)).toBe(true);
  });

  it("previews a gesture and commits exactly what it showed", async () => {
    await adoptFixture();
    useEditor.getState().click(15, 15, false);
    const command = { kind: "translate", dx: 11, dy: 7 } as const;

    useEditor.getState().preview(command);
    let state = useEditor.getState();
    expect(state.previewing).toEqual(command);
    // The engine's box follows the preview; the React mirror and the history
    // are deliberately not re-read during a drag (that is the point of a
    // preview: a gesture does not copy the node list per frame).
    expect(state.selectionBox).toEqual({ x0: 10, y0: 10, x1: 30, y1: 30 });
    expect(state.history.undoable).toBe(0);
    const session = editorSession();
    expect(session?.boundsOf(1)).toEqual({ x0: 21, y0: 17, x1: 41, y1: 37 });
    expect(session?.selectionBounds()).toEqual({ x0: 21, y0: 17, x1: 41, y1: 37 });

    useEditor.getState().commit(command);
    state = useEditor.getState();
    expect(state.previewing).toBeNull();
    expect(state.history.undoable).toBe(1);
    expect(state.history.undoLabel).toBe("move"); // the engine's own label
    expect(state.selectionBox).toEqual({ x0: 21, y0: 17, x1: 41, y1: 37 });
    expect(state.nodes[0].m.slice(4)).toEqual([11, 7]);

    // Cancelling drops the engine's preview and the guides with it.
    useEditor.getState().preview({ kind: "translate", dx: 100, dy: 0 });
    useEditor.getState().setGuides([{ axis: 0, kind: 2, position: 1, from: 0, to: 10 }]);
    useEditor.getState().cancelPreview();
    state = useEditor.getState();
    expect(state.previewing).toBeNull();
    expect(state.guides).toEqual([]);
    expect(session?.boundsOf(1)).toEqual({ x0: 21, y0: 17, x1: 41, y1: 37 });
  });

  it("drives align, arrange, resize and rotate through the panels", async () => {
    await adoptFixture();
    useEditor.getState().selectAll();
    useEditor.getState().align("selection", "left");
    let state = useEditor.getState();
    // Node 1 is already at x0 = 10; node 2 moves from 60 to 10.
    expect(state.nodes[1].m.slice(4)).toEqual([10, 10]);
    expect(state.note).toMatch(/align left · 1 node/);
    useEditor.getState().undo();

    // Node 1 is the back-most node, so sending it further back is refused…
    useEditor.getState().click(15, 15, false);
    useEditor.getState().arrange("back");
    state = useEditor.getState();
    expect(state.nodes[0].id).toBe(1);
    expect(state.history.undoable).toBe(0);
    expect(state.note).toMatch(/nothing would change/);
    // …and bringing it to the front really does reorder the document.
    useEditor.getState().arrange("front");
    state = useEditor.getState();
    expect(state.history.undoLabel).toBe("arrange"); // the engine's own label
    expect(state.nodes[1].id).toBe(1);
    useEditor.getState().undo();
    expect(useEditor.getState().nodes[0].id).toBe(1);

    // Resizing is a ScaleXY about the selection's top-left corner.
    useEditor.getState().selectAll();
    useEditor.getState().click(15, 15, false);
    useEditor.getState().resize({ width: 140, height: 40 });
    state = useEditor.getState();
    expect(state.note).toMatch(/resize · 1 node/);
    expect(state.selectionBox).toEqual({ x0: 10, y0: 10, x1: 150, y1: 50 });
    useEditor.getState().undo();

    useEditor.getState().click(15, 15, false);
    useEditor.getState().rotate(90);
    state = useEditor.getState();
    expect(state.note).toMatch(/rotate · 1 node/);
    // A 20x20 square turned 90° about its centre is the same square — to
    // within the f32 rounding a real rotation introduces.
    expect(state.selectionBox?.x0).toBeCloseTo(10, 4);
    expect(state.selectionBox?.y0).toBeCloseTo(10, 4);
    expect(state.selectionBox?.x1).toBeCloseTo(30, 4);
    expect(state.selectionBox?.y1).toBeCloseTo(30, 4);
    useEditor.getState().undo();

    // Rotating without a selection is a note, not a crash.
    useEditor.getState().clearSelection();
    useEditor.getState().rotate(90);
    expect(useEditor.getState().note).toMatch(/select something first/);
    console.log(
      "evidence: editor panels — align, arrange, resize and rotate driven through the store: every " +
        "command is one history step, and a refusal is reported instead of thrown",
    );
  });

  it("groups a selection so one click drags all of it", async () => {
    await adoptFixture();
    useEditor.getState().selectAll();
    useEditor.getState().group();
    let state = useEditor.getState();
    expect(state.history.undoLabel).toBe("group");
    expect(state.nodes.map((node) => node.group)).toEqual([1, 1]);

    // Clicking one member selects the whole group, and a move carries it along.
    useEditor.getState().click(15, 15, false);
    state = useEditor.getState();
    expect(state.selection).toEqual([1, 2]);
    useEditor.getState().nudge(4, 0);
    state = useEditor.getState();
    expect(state.nodes.map((node) => node.m[4])).toEqual([4, 64]);
    useEditor.getState().undo();

    useEditor.getState().click(15, 15, false);
    useEditor.getState().ungroup();
    state = useEditor.getState();
    expect(state.nodes.map((node) => node.group)).toEqual([0, 0]);
    useEditor.getState().click(15, 15, false);
    expect(useEditor.getState().selection).toEqual([1]);
  });

  it("keeps the snap settings and reports them", async () => {
    await adoptFixture();
    useEditor.getState().setSnap({ ...SNAP_DEFAULTS }); // the store is a singleton
    expect(useEditor.getState().snap).toEqual({
      canvas: true,
      nodes: true,
      grid: false,
      gridStep: 8,
      tolerance: 6,
    });
    useEditor.getState().setSnap({ grid: true, gridStep: 16 });
    const state = useEditor.getState();
    expect(state.snap.grid).toBe(true);
    expect(state.snap.gridStep).toBe(16);
    expect(SNAP.GRID).toBe(4);
  });
});
