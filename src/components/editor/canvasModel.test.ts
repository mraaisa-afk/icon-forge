import { describe, expect, it } from "vitest";

import { KIND, type NodeSpec } from "../../wasm/abi";

import {
  applyAffine,
  boxAfterCommand,
  commandAffine,
  fitView,
  gestureFor,
  handlePositions,
  hitHandle,
  isDrag,
  marqueeBox,
  cubicAt,
  hitPoint,
  hitSegment,
  invertAffine,
  nodePoints,
  nodeWithPointMoved,
  rotateCommandFor,
  scaleCommandFor,
  snapFlags,
  SNAP_DEFAULTS,
  toDocPoint,
  toScreenPoint,
  wrapDegrees,
} from "./canvasModel";

describe("fitView", () => {
  it("fits a wide document to the width and centres it", () => {
    const view = fitView(200, 200, 100, 50, 10);
    expect(view.scale).toBeCloseTo(1.8, 6); // (200-20)/100
    expect(view.offsetX).toBeCloseTo(10, 6);
    expect(view.offsetY).toBeCloseTo((200 - 50 * 1.8) / 2, 6);
  });

  it("fits a tall document to the height", () => {
    const view = fitView(200, 200, 50, 100, 10);
    expect(view.scale).toBeCloseTo(1.8, 6);
    expect(view.offsetY).toBeCloseTo(10, 6);
  });

  it("falls back to whole pixels for an empty or impossible box", () => {
    expect(fitView(0, 0, 100, 100, 10)).toEqual({ scale: 1, offsetX: 0, offsetY: 0 });
    expect(fitView(200, 200, 0, 0, 10)).toEqual({ scale: 1, offsetX: 0, offsetY: 0 });
    // A margin bigger than the canvas would otherwise give a negative scale.
    expect(fitView(10, 10, 100, 100, 20).scale).toBe(1);
  });

  it("round-trips a point through screen and document space", () => {
    const view = fitView(640, 480, 256, 128, 12);
    const doc = toDocPoint(300, 200, view);
    const screen = toScreenPoint(doc.x, doc.y, view);
    expect(screen.x).toBeCloseTo(300, 6);
    expect(screen.y).toBeCloseTo(200, 6);
  });

  it("maps the document's top-left corner to the margin", () => {
    const view = fitView(200, 200, 100, 100, 10);
    const corner = toScreenPoint(0, 0, view);
    expect(corner).toEqual({ x: view.offsetX, y: view.offsetY });
    expect(toDocPoint(corner.x, corner.y, view)).toEqual({ x: 0, y: 0 });
  });
});

describe("marqueeBox", () => {
  it("normalises corners dragged in any direction", () => {
    expect(marqueeBox({ x: 10, y: 20 }, { x: 4, y: 30 })).toEqual({
      x0: 4,
      y0: 20,
      x1: 10,
      y1: 30,
    });
  });
});

describe("gestureFor", () => {
  it("sweeps on empty space", () => {
    expect(gestureFor(0, [1, 2], false)).toEqual({ kind: "marquee" });
    expect(gestureFor(0, [], true)).toEqual({ kind: "marquee" });
  });

  it("moves an already-selected node without touching the selection", () => {
    expect(gestureFor(2, [1, 2], false)).toEqual({ kind: "move", selectFirst: "none" });
  });

  it("selects an unselected node first, replacing or toggling per the modifier", () => {
    expect(gestureFor(3, [1, 2], false)).toEqual({ kind: "move", selectFirst: "only" });
    expect(gestureFor(3, [1, 2], true)).toEqual({ kind: "move", selectFirst: "toggle" });
  });
});

describe("isDrag", () => {
  it("separates a click from a nudge", () => {
    expect(isDrag(0, 0)).toBe(false);
    expect(isDrag(0.3, 0.3)).toBe(false);
    expect(isDrag(1, 0)).toBe(true);
  });
});

const VIEW = { scale: 2, offsetX: 10, offsetY: 5 };
const BOX = { x0: 10, y0: 20, x1: 60, y1: 70 };

describe("transform handles", () => {
  it("puts the eight handles on the box corners and edges, and the grip above", () => {
    const handles = handlePositions(BOX, VIEW);
    // Corners map through the view; the rotate grip floats above the top edge.
    expect(handles.nw).toEqual(toScreenPoint(10, 20, VIEW));
    expect(handles.se).toEqual(toScreenPoint(60, 70, VIEW));
    expect(handles.e.x).toBe(toScreenPoint(60, 45, VIEW).x);
    expect(handles.n.y).toBe(handles.nw.y);
    expect(handles.rotate.x).toBeCloseTo((handles.nw.x + handles.se.x) / 2, 6);
    expect(handles.rotate.y).toBeLessThan(handles.nw.y);
  });

  it("grabs the nearest handle and nothing far away", () => {
    const handles = handlePositions(BOX, VIEW);
    expect(hitHandle(handles.ne, BOX, VIEW)).toBe("ne");
    expect(hitHandle({ x: handles.rotate.x, y: handles.rotate.y }, BOX, VIEW)).toBe("rotate");
    expect(hitHandle({ x: 0, y: 0 }, BOX, VIEW)).toBeNull();
    // Just inside the grab radius counts, just outside does not.
    const near = { x: handles.se.x + 8, y: handles.se.y };
    expect(hitHandle(near, BOX, VIEW)).toBe("se");
    expect(hitHandle({ x: handles.se.x + 40, y: handles.se.y + 40 }, BOX, VIEW)).toBeNull();
  });

  it("scales a corner uniformly about the opposite corner", () => {
    // Dragging the south-east corner of a 50x50 box twice as far from the
    // north-west pivot doubles it.
    const command = scaleCommandFor("se", BOX, { x: 110, y: 120 }, { uniform: true });
    expect(command).toEqual({ kind: "scale", factor: 2, pivot: { x: 10, y: 20 } });
    // The uniform factor follows the diagonal, so the aspect cannot drift.
    const skewed = scaleCommandFor("se", BOX, { x: 110, y: 20 }, { uniform: true });
    expect(skewed?.kind).toBe("scale");
    if (skewed?.kind === "scale") expect(skewed.factor).toBeCloseTo(Math.SQRT2, 6);
    // The pivot (the opposite corner) never moves.
    expect(applyAffine(commandAffine(skewed!)!, { x: 10, y: 20 })).toEqual({ x: 10, y: 20 });
  });

  it("scales freely with Shift, and one axis only from an edge handle", () => {
    const free = scaleCommandFor("se", BOX, { x: 110, y: 45 }, { uniform: false });
    expect(free).toEqual({ kind: "scaleXY", sx: 2, sy: 0.5, pivot: { x: 10, y: 20 } });
    const edge = scaleCommandFor("e", BOX, { x: 110, y: 999 }, { uniform: false });
    expect(edge).toEqual({ kind: "scaleXY", sx: 2, sy: 1, pivot: { x: 10, y: 45 } });
    // The top edge dragged twice as far from the bottom edge stretches y only.
    const vertical = scaleCommandFor("n", BOX, { x: -999, y: -30 }, { uniform: false });
    expect(vertical).toEqual({ kind: "scaleXY", sx: 1, sy: 2, pivot: { x: 35, y: 70 } });
  });

  it("scales about the centre when asked, and refuses degenerate drags", () => {
    // From the centre the same drag is three times the half-diagonal out.
    const centred = scaleCommandFor("se", BOX, { x: 110, y: 120 }, { uniform: true, fromCenter: true });
    expect(centred).toEqual({ kind: "scale", factor: 3, pivot: { x: 35, y: 45 } });
    // Collapsing the box onto its pivot has no scale command that means anything.
    expect(scaleCommandFor("se", BOX, { x: 10, y: 20 }, { uniform: true })).toBeNull();
    expect(scaleCommandFor("se", BOX, { x: 10, y: 20 }, { uniform: false })).toBeNull();
    expect(scaleCommandFor("se", BOX, { x: 60, y: 70 }, { uniform: false })).toBeNull();
    expect(scaleCommandFor("rotate", BOX, { x: 0, y: 0 }, { uniform: true })).toBeNull();
  });

  it("rotates about the box centre, snapping to 15 degrees with Shift", () => {
    const centre = { x: 35, y: 45 };
    const from = { x: 35, y: 95 }; // straight below the centre
    // The grip starts below the centre and ends to its right: a quarter turn.
    const quarter = rotateCommandFor(BOX, from, { x: 85, y: 45 });
    expect(quarter).toEqual({ kind: "rotate", degrees: -90, pivot: centre });
    const snapped = rotateCommandFor(BOX, from, { x: 80, y: 40 }, { snap: true });
    expect(snapped?.kind).toBe("rotate");
    if (snapped?.kind === "rotate") expect(Math.abs(snapped.degrees % 15)).toBe(0);
    // A drag that does not turn anything, and a pivot-less box, do nothing.
    expect(rotateCommandFor(BOX, from, from)).toBeNull();
    expect(wrapDegrees(370)).toBe(10);
    expect(wrapDegrees(-190)).toBe(170);
  });
});

describe("local previews", () => {
  it("builds the affine for each transform command", () => {
    expect(commandAffine({ kind: "translate", dx: 3, dy: -4 })).toEqual([1, 0, 0, 1, 3, -4]);
    expect(commandAffine({ kind: "scale", factor: 2, pivot: { x: 10, y: 20 } })).toEqual([
      2, 0, 0, 2, -10, -20,
    ]);
    expect(
      commandAffine({ kind: "scaleXY", sx: 2, sy: 0.5, pivot: { x: 10, y: 20 } }),
    ).toEqual([2, 0, 0, 0.5, -10, 10]);
    const rotate = commandAffine({ kind: "rotate", degrees: 90, pivot: { x: 0, y: 0 } });
    expect(rotate?.[0]).toBeCloseTo(0, 6);
    expect(rotate?.[1]).toBeCloseTo(1, 6);
    expect(rotate?.[2]).toBeCloseTo(-1, 6);
    expect(rotate?.[3]).toBeCloseTo(0, 6);
    // Commands that are not transforms have no affine to draw with.
    expect(commandAffine({ kind: "delete" })).toBeNull();
    expect(commandAffine({ kind: "group" })).toBeNull();
  });

  it("maps points and boxes through an affine", () => {
    const m = commandAffine({ kind: "scaleXY", sx: 2, sy: 0.5, pivot: { x: 10, y: 20 } });
    if (!m) throw new Error("a scaleXY has an affine");
    expect(applyAffine(m, { x: 10, y: 20 })).toEqual({ x: 10, y: 20 }); // the pivot stays put
    expect(applyAffine(m, { x: 20, y: 40 })).toEqual({ x: 30, y: 30 });
    expect(boxAfterCommand(BOX, { kind: "translate", dx: 5, dy: 5 })).toEqual({
      x0: 15,
      y0: 25,
      x1: 65,
      y1: 75,
    });
    // A rotation turns the box into the box around the turned corners.
    const turned = boxAfterCommand(
      { x0: 0, y0: 0, x1: 10, y1: 10 },
      { kind: "rotate", degrees: 45, pivot: { x: 0, y: 0 } },
    );
    // A 10x10 square turned 45° about its top-left corner spans x −7.07..7.07
    // and y 0..14.14.
    expect(turned.y1).toBeCloseTo(Math.SQRT2 * 10, 4);
    expect(turned.x1).toBeCloseTo(Math.SQRT2 * 5, 4);
    expect(turned.x0).toBeCloseTo(-Math.SQRT2 * 5, 4);
    // Commands that are not transforms leave the box alone.
    expect(boxAfterCommand(BOX, { kind: "delete" })).toEqual(BOX);
  });

  it("turns the snap settings into the ABI's flag mask", () => {
    expect(snapFlags(SNAP_DEFAULTS)).toBe(3); // canvas + nodes
    expect(snapFlags({ ...SNAP_DEFAULTS, grid: true })).toBe(7);
    expect(snapFlags({ ...SNAP_DEFAULTS, canvas: false, nodes: false })).toBe(0);
    expect(SNAP_DEFAULTS.gridStep).toBe(8);
    expect(SNAP_DEFAULTS.tolerance).toBeGreaterThan(0);
  });
});

describe("node editing", () => {
  const SQUARE: NodeSpec = {
    id: 1,
    m: [1, 0, 0, 1, 0, 0],
    fill: [255, 255, 255, 255],
    visible: true,
    path: [
      {
        start: { x: 10, y: 10 },
        closed: true,
        segs: [
          { kind: KIND.LINE, to: { x: 30, y: 10 } },
          { kind: KIND.LINE, to: { x: 30, y: 30 } },
          { kind: KIND.LINE, to: { x: 10, y: 30 } },
        ],
      },
    ],
  };

  const CURVE: NodeSpec = {
    ...SQUARE,
    id: 2,
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
  };

  it("lists every vertex, then the control handles of the cubics", () => {
    const points = nodePoints(SQUARE);
    // Four vertices of a closed square, no handles (all three segments are lines).
    expect(points).toHaveLength(4);
    expect(points.map((point) => point.at.of)).toEqual(["vertex", "vertex", "vertex", "vertex"]);
    expect(points[3].to).toEqual({ x: 10, y: 30 });

    const curve = nodePoints(CURVE);
    expect(curve).toHaveLength(4); // start, end, c1, c2
    expect(curve[2].at).toEqual({ of: "handle", subpath: 0, segment: 0, handle: "c1" });
    // A handle is drawn as a lever from the vertex it turns about.
    expect(curve[2].from).toEqual({ x: 0, y: 0 });
    expect(curve[3].from).toEqual({ x: 10, y: 0 });
    expect(curve[3].to).toEqual({ x: 10, y: 10 });
  });

  it("prefers a vertex over a handle when both are in reach", () => {
    const points = nodePoints(CURVE);
    // (0, 0) is the start vertex and the c1 lever's anchor: the vertex wins.
    expect(hitPoint(points, { x: 0.5, y: 0.5 })?.at).toEqual({
      of: "vertex",
      subpath: 0,
      vertex: 0,
    });
    // Just outside the grab radius of anything is nothing.
    expect(hitPoint(points, { x: 5, y: 5 })).toBeNull();
  });

  it("finds the nearest place on an outline for an insert", () => {
    const onLine = hitSegment(SQUARE, { x: 20, y: 10.2 });
    expect(onLine).toEqual({ of: "segment", subpath: 0, segment: 0, t: 0.5 });

    const onCurve = hitSegment(CURVE, { x: 5, y: 7.5 });
    if (!onCurve) throw new Error("the curve is within reach");
    expect(onCurve.segment).toBe(0);
    // The parameter stays strictly inside the segment, because the engine's
    // `insertPoint` refuses the segment's own ends.
    expect(onCurve.t).toBeGreaterThan(0);
    expect(onCurve.t).toBeLessThan(1);
    expect(cubicAt({ x: 0, y: 0 }, { x: 0, y: 10 }, { x: 10, y: 10 }, { x: 10, y: 0 }, onCurve.t).y)
      .toBeCloseTo(7.5, 1);

    // An outline that is nowhere near is not a hit.
    expect(hitSegment(SQUARE, { x: 100, y: 100 })).toBeNull();
  });

  it("moves the point the drag grabbed, and the handles that hang off it", () => {
    // Moving a middle vertex drags the incoming and outgoing handles with it.
    const moved = nodeWithPointMoved(CURVE, { of: "vertex", subpath: 0, vertex: 1 }, { x: 20, y: 5 });
    const seg = moved.path[0].segs[0];
    if (seg.kind !== KIND.CUBIC) throw new Error("the segment is a cubic");
    expect(seg.to).toEqual({ x: 20, y: 5 });
    expect(seg.c2).toEqual({ x: 20, y: 15 }); // moved by the same delta as its vertex
    expect(seg.c1).toEqual({ x: 0, y: 10 }); // untouched: it hangs off the other end

    // Moving the start vertex carries the handle that leaves it.
    const start = nodeWithPointMoved(CURVE, { of: "vertex", subpath: 0, vertex: 0 }, { x: -5, y: 2 });
    const first = start.path[0].segs[0];
    if (first.kind !== KIND.CUBIC) throw new Error("the segment is a cubic");
    expect(start.path[0].start).toEqual({ x: -5, y: 2 });
    expect(first.c1).toEqual({ x: -5, y: 12 });

    // A line segment has no handles to drag, so only its end moves.
    const line = nodeWithPointMoved(SQUARE, { of: "vertex", subpath: 0, vertex: 1 }, { x: 40, y: 12 });
    expect(line.path[0].segs[0]).toEqual({ kind: KIND.LINE, to: { x: 40, y: 12 } });
    expect(line.path[0].segs[1].to).toEqual({ x: 30, y: 30 });

    // A handle drag moves only that handle.
    const handle = nodeWithPointMoved(
      CURVE,
      { of: "handle", subpath: 0, segment: 0, handle: "c2" },
      { x: 12, y: 14 },
    );
    const dragged = handle.path[0].segs[0];
    if (dragged.kind !== KIND.CUBIC) throw new Error("the segment is a cubic");
    expect(dragged.c2).toEqual({ x: 12, y: 14 });
    expect(dragged.c1).toEqual({ x: 0, y: 10 });
  });

  it("works in the node's own space when the node is transformed", () => {
    // The path is local; the document-space point comes back through the
    // inverse of the node's affine, so a scaled node still lands under the
    // pointer.
    const scaled: NodeSpec = { ...SQUARE, m: [2, 0, 0, 2, 5, 5] };
    expect(nodePoints(scaled)[1].to).toEqual({ x: 65, y: 25 });
    const moved = nodeWithPointMoved(scaled, { of: "vertex", subpath: 0, vertex: 1 }, { x: 65, y: 25 });
    expect(moved.path[0].segs[0].to).toEqual({ x: 30, y: 10 }); // local coordinates

    // A collapsed affine has no inverse to write through, so nothing is
    // guessed: the drag writes the document-space point unchanged.
    const flat: NodeSpec = { ...SQUARE, m: [0, 0, 0, 0, 1, 1] };
    expect(invertAffine(flat.m as [number, number, number, number, number, number], { x: 9, y: 9 })).toBeNull();
    expect(nodeWithPointMoved(flat, { of: "vertex", subpath: 0, vertex: 1 }, { x: 9, y: 9 }).path[0].segs[0].to)
      .toEqual({ x: 9, y: 9 });
  });
});
