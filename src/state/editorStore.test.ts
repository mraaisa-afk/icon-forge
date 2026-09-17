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
import { KIND, type NodeSpec, type Subpath } from "../wasm/abi";
import { artifactAvailable as available, loadArtifact } from "../wasm/artifact";
import { useEditor, nodeFromSvg, EDITOR_ICON_LIMIT } from "./editorStore";

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
