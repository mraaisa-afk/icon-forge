/**
 * Integration tests for the editor adapter, driven by the **real** WebAssembly
 * artifact when one is available.
 *
 * The artifact is a build product (CI builds it on the Windows leg, and
 * `cargo build --target wasm32-unknown-unknown -p isg-wasm` reproduces it
 * locally), so the suite is skipped when it is absent — with a warning, because
 * a silently skipped integration suite is how an ABI drifts.
 */

import { describe, expect, it } from "vitest";

import { ERR, KIND, type EditorCommand, type NodeSpec, type Subpath } from "./abi";
import { EditorSession } from "./editor";
import { artifactAvailable as available, loadArtifact } from "./artifact";

const bytes = loadArtifact() ?? new Uint8Array(new ArrayBuffer(0));

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

function fixture(): NodeSpec[] {
  return [
    { id: 1, m: [1, 0, 0, 1, 0, 0], fill: [255, 0, 0, 255], visible: true, path: [square(10, 10, 20)] },
    { id: 2, m: [1, 0, 0, 1, 50, 20], fill: [0, 255, 0, 255], visible: true, path: [square(0, 0, 30)] },
    {
      id: 3,
      m: [2, 0, 0, -1, 0, 0],
      fill: [0, 0, 255, 255],
      visible: false,
      path: [
        {
          start: { x: 0, y: 0 },
          closed: false,
          segs: [
            {
              kind: KIND.CUBIC,
              c1: { x: 0, y: 10 },
              c2: { x: 10, y: 10 },
              to: { x: 10, y: 0 },
            },
          ],
        },
      ],
    },
  ];
}

async function freshSession(): Promise<EditorSession> {
  const session = await EditorSession.fromBytes(bytes);
  const loaded = session.loadDocument(200, 120, fixture());
  expect(loaded.ok).toBe(true);
  return session;
}

describe.skipIf(!available)("EditorSession against the real module", () => {
  it("checks the ABI level it was built against", async () => {
    const session = await EditorSession.fromBytes(bytes);
    expect(session.abiVersion).toBe(1);
    expect(session.compatible).toBe(true);
    console.log(
      `evidence: editor module — ${bytes.length} bytes, ABI v${session.abiVersion}, memory ${session.exports.memory.buffer.byteLength} bytes at first call`,
    );
  });

  it("rejects a module that is not the editor", async () => {
    // The smallest valid module: no memory, no entry points.
    const empty = new Uint8Array([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]);
    await expect(EditorSession.fromBytes(empty)).rejects.toThrow(/not the Icon Forge editor/);
  });

  it("loads a document and reports it", async () => {
    const session = await freshSession();
    expect(session.nodeCount()).toBe(3);
    expect(session.segmentCount()).toBe(7);
    expect(session.docSize()).toEqual({ width: 200, height: 120 });
    expect(session.revision).toBeGreaterThan(0);
    session.close();
    expect(session.nodeCount()).toBe(0);
  });

  it("round-trips every node field through NODE_SYNC", async () => {
    const session = await freshSession();
    const nodes = session.nodes();
    expect(nodes).toHaveLength(3);
    for (const [index, loaded] of fixture().entries()) {
      const back = nodes[index];
      expect(back.id).toBe(loaded.id);
      expect(back.fill).toEqual(loaded.fill);
      expect(back.visible).toBe(loaded.visible);
      expect(back.m.map(Math.fround)).toEqual(loaded.m.map(Math.fround));
      // Local geometry travels unchanged; the transform carries the placement.
      expect(back.path).toEqual(loaded.path);
    }
  });

  it("flushes a node's placed path and box", async () => {
    const session = await freshSession();
    const placed = session.pathOf(2);
    expect(placed).not.toBeNull();
    expect(placed?.[0].start).toEqual({ x: 50, y: 20 });
    expect(session.boundsOf(2)).toEqual({ x0: 50, y0: 20, x1: 80, y1: 50 });
    expect(session.pathOf(999)).toBeNull();
    expect(session.lastError).toBe(ERR.BAD_ARGUMENT);
    expect(session.lastErrorMessage).toMatch(/bad argument/);
  });

  it("picks, marquees and selects", async () => {
    const session = await freshSession();
    expect(session.pick(15, 15)).toBe(1);
    expect(session.pick(55, 25)).toBe(2);
    expect(session.pick(190, 110)).toBe(0);
    expect(session.marquee({ x0: 0, y0: 0, x1: 45, y1: 45 })).toEqual([1]);
    expect(session.marquee({ x0: 0, y0: 0, x1: 200, y1: 120 })).toEqual([1, 2]);

    expect(session.selectOnly(2)).toBe(1);
    expect(session.selection()).toEqual([2]);
    expect(session.selectAdd(1)).toBe(2);
    expect(session.selection()).toEqual([1, 2]);
    expect(session.selectionBounds()).toEqual({ x0: 10, y0: 10, x1: 80, y1: 50 });
    expect(session.selectToggle(1)).toBe(1);
    expect(session.selectAll()).toBe(3);
    session.clearSelection();
    expect(session.selection()).toEqual([]);
  });

  it("keeps the hit tolerance as a live setting", async () => {
    const session = await freshSession();
    const original = session.tolerance;
    expect(original).toBeGreaterThan(0);
    session.tolerance = 7.5;
    expect(session.tolerance).toBe(7.5);
    session.tolerance = -1; // refused, leaving the previous value in force
    // The error is read first: any later call would replace it with its own code.
    expect(session.lastError).toBe(ERR.BAD_ARGUMENT);
    expect(session.tolerance).toBe(7.5);
  });

  it("applies a drag, undoes it exactly, and redoes it exactly", async () => {
    const session = await freshSession();
    session.selectOnly(1);
    const before = session.nodes();
    const applied = session.apply({ kind: "translate", dx: 5.5, dy: -3.25 });
    expect(applied.ok).toBe(true);
    expect(session.boundsOf(1)).toEqual({ x0: 15.5, y0: 6.75, x1: 35.5, y1: 26.75 });
    const moved = session.nodes();

    const undone = session.undo();
    expect(undone.ok && undone.value).toBe("move");
    expect(session.nodes()).toEqual(before);
    const redone = session.redo();
    expect(redone.ok && redone.value).toBe("move");
    expect(session.nodes()).toEqual(moved);
    // The history view the toolbar renders.
    const history = session.history();
    expect(history.canUndo).toBe(true);
    expect(history.canRedo).toBe(false);
    expect(history.undoable).toBe(1);
    expect(history.undoLabel).toBe("move");
    expect(history.lastLabel).toBe("move");
    session.close();
  });

  it("surfaces engine refusals as codes, not exceptions", async () => {
    const session = await freshSession();
    session.clearSelection();
    const noSelection = session.apply({ kind: "delete" });
    expect(noSelection.ok).toBe(false);
    if (!noSelection.ok) {
      expect(noSelection.error).toBe(ERR.NO_SELECTION);
      expect(noSelection.message).toMatch(/select/i);
    }
    session.selectOnly(1);
    const zero = session.apply({ kind: "translate", dx: 0, dy: 0 });
    expect(zero.ok).toBe(false);
    if (!zero.ok) expect(zero.error).toBe(ERR.NO_OP);
    const transparent = session.apply({ kind: "fill", rgba: [1, 2, 3, 0] });
    expect(transparent.ok).toBe(false);
    if (!transparent.ok) expect(transparent.error).toBe(ERR.TRANSPARENT);
    const nothingToUndo = session.undo();
    expect(nothingToUndo.ok).toBe(false);
    if (!nothingToUndo.ok) expect(nothingToUndo.error).toBe(ERR.NO_HISTORY);
  });

  it("performs the Phase 4 exit criterion through the adapter", async () => {
    const session = await freshSession();
    const snapshot = (): string =>
      JSON.stringify([
        session.nodes().map((node) => [node.id, node.m, node.fill, node.visible, node.path]),
        session.selection(),
      ]);
    let seed = 0x2f6e2b1;
    const rnd = (): number => {
      seed = (seed * 1103515245 + 12345) & 0x7fffffff;
      return seed / 0x7fffffff;
    };
    const commands: Array<() => EditorCommand> = [
      () => ({ kind: "translate", dx: rnd() * 8 - 4, dy: rnd() * 8 - 4 }),
      () => ({ kind: "scale", factor: 0.5 + rnd() * 2, pivot: { x: 20, y: 20 } }),
      () => ({ kind: "rotate", degrees: rnd() * 90, pivot: { x: 0, y: 0 } }),
      () => ({ kind: "duplicate", dx: 3, dy: 4 }),
      () => ({ kind: "fill", rgba: [Math.floor(rnd() * 255), 20, 30, 255] }),
      () => ({ kind: "visible", to: rnd() < 0.5 }),
      () => ({ kind: "reorder", up: rnd() < 0.5 }),
      () => ({ kind: "center" }),
      () => ({ kind: "delete" }),
    ];
    let applied = 0;
    for (let step = 0; step < 1000; step++) {
      if (rnd() < 0.6) {
        session.selectAll();
      } else {
        const ids = session.nodes().map((node) => node.id);
        if (ids.length === 0) break;
        session.selectOnly(ids[Math.floor(rnd() * ids.length)]);
      }
      const command = commands[Math.floor(rnd() * commands.length)]();
      const before = snapshot();
      const result = session.apply(command);
      if (!result.ok) {
        // A refusal must leave the state untouched.
        expect(snapshot()).toBe(before);
        continue;
      }
      applied += 1;
      const after = snapshot();
      session.undo();
      expect(snapshot()).toBe(before);
      session.redo();
      expect(snapshot()).toBe(after);
      session.undo();
    }
    expect(applied).toBeGreaterThan(600);
    console.log(
      `evidence: editor adapter property — ${applied} edits applied over 1000 randomised iterations, zero undo/redo divergences`,
    );
    session.close();
  });

  it("keeps a 5000-path sheet inside one frame's geometry budget", async () => {
    const session = await EditorSession.fromBytes(bytes);
    // 1000 icons x 5 paths = 5000 paths — the sheet size the Phase 4 exit
    // criterion names. Rasterisation belongs to the webview, so the budget is
    // measured where this project owns the cost: the geometry pass the canvas
    // runs (sync once, then walk the live buffer with no per-node allocation).
    const P = 5;
    const N = 1000;
    const nodes: NodeSpec[] = Array.from({ length: N }, (_, n) => ({
      id: n + 1,
      m: [1, 0, 0, 1, (n % 32) * 40, Math.floor(n / 32) * 40],
      fill: [200, 200, 200, 255],
      visible: true,
      path: Array.from({ length: P }, (_, p) => square(p * 4, p * 4, 8)),
    }));
    const loaded = session.loadDocument(1280, 1280, nodes);
    expect(loaded.ok).toBe(true);
    expect(session.segmentCount()).toBe(N * P * 3);

    const draw = (): number => {
      let seen = 0;
      session.forEachNode((node) => {
        node.eachSubpath((sub) => {
          sub.eachSegment(() => {
            seen += 2;
          });
        });
      });
      return seen;
    };

    // The first pass pays for the sync (the module copies the document into its
    // output table) and for the module's own first-touch warm-up — that is the
    // cost of committing an edit, not of a frame, so it is measured separately
    // after one warm-up round.
    const geometry = draw();
    const commits: number[] = [];
    for (let i = 0; i < 5; i++) {
      session.invalidateNodes();
      const started = performance.now();
      expect(draw()).toBe(geometry);
      commits.push(performance.now() - started);
    }
    commits.sort((a, b) => a - b);
    const commitMs = commits[commits.length >> 1];

    // Every later repaint of an unchanged document: the buffer walk alone.
    const frames: number[] = [];
    for (let frame = 0; frame < 40; frame++) {
      const started = performance.now();
      expect(draw()).toBe(geometry);
      frames.push(performance.now() - started);
    }
    frames.sort((a, b) => a - b);
    const median = frames[frames.length >> 1];
    const p90 = frames[Math.floor(frames.length * 0.9)];
    const worst = frames[frames.length - 1];
    // 60 fps leaves 16.6 ms for everything: the median frame must leave most of
    // that to the webview's own rasterisation, and essentially every frame must
    // still fit inside the budget. The single worst frame is reported but given
    // two frames' worth of headroom, because a CI runner will occasionally lose
    // the CPU to something else and a gate that flakes is a gate nobody reads.
    expect(median).toBeLessThan(8);
    expect(p90).toBeLessThan(16.6);
    expect(worst).toBeLessThan(33);
    // A committed edit is a one-off, not a frame: two frames is the bar.
    expect(commitMs).toBeLessThan(33);
    console.log(
      `evidence: editor frame budget — ${N * P} paths (${geometry / 2} segments): ` +
        `edit commit ${commitMs.toFixed(2)} ms, frames median ${median.toFixed(2)} / ` +
        `p90 ${p90.toFixed(2)} / worst ${worst.toFixed(2)} ms of the 16.6 ms budget`,
    );
    session.close();
  });

  it("reloads cleanly after a close (history never leaks across documents)", async () => {
    const session = await freshSession();
    session.selectAll();
    session.apply({ kind: "translate", dx: 1, dy: 1 });
    session.close();
    expect(session.history().canUndo).toBe(false);
    expect(session.loadDocument(50, 50, [fixture()[0]]).ok).toBe(true);
    expect(session.nodeCount()).toBe(1);
    expect(session.history().canUndo).toBe(false);
    expect(session.selection()).toEqual([]);
  });
});
