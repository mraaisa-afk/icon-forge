/**
 * Pure geometry for the editor canvas.
 *
 * Kept out of the component so the mapping between screen pixels and document
 * units is testable without a DOM: getting that wrong means clicks select the
 * wrong icon, which is exactly the kind of bug a canvas test would otherwise
 * miss.
 */

import {
  GUIDE_KIND,
  KIND,
  SNAP,
  type Box,
  type EditorCommand,
  type HandleAddress,
  type NodeSpec,
  type Point,
  type SegmentAddress,
  type Subpath,
  type VertexAddress,
} from "../../wasm/abi";

/** Where the document sits inside the canvas, and how big one unit is. */
export interface View {
  /** Document units per canvas pixel. */
  scale: number;
  offsetX: number;
  offsetY: number;
}

/** Fits a document into a canvas box, leaving a margin all round. */
export function fitView(
  canvasWidth: number,
  canvasHeight: number,
  docWidth: number,
  docHeight: number,
  margin: number,
): View {
  if (docWidth <= 0 || docHeight <= 0 || canvasWidth <= 0 || canvasHeight <= 0) {
    return { scale: 1, offsetX: 0, offsetY: 0 };
  }
  const scale = Math.min(
    (canvasWidth - margin * 2) / docWidth,
    (canvasHeight - margin * 2) / docHeight,
  );
  const fit = scale > 0 ? scale : 1;
  return {
    scale: fit,
    offsetX: (canvasWidth - docWidth * fit) / 2,
    offsetY: (canvasHeight - docHeight * fit) / 2,
  };
}

/** Screen point (relative to the canvas box) to document point. */
export function toDocPoint(
  x: number,
  y: number,
  view: View,
): { x: number; y: number } {
  return { x: (x - view.offsetX) / view.scale, y: (y - view.offsetY) / view.scale };
}

/** Document point to screen point (relative to the canvas box). */
export function toScreenPoint(x: number, y: number, view: View): { x: number; y: number } {
  return { x: x * view.scale + view.offsetX, y: y * view.scale + view.offsetY };
}

/** Normalises two drag corners into a box (the ABI accepts corners in any order). */
export function marqueeBox(
  a: { x: number; y: number },
  b: { x: number; y: number },
): Box {
  return {
    x0: Math.min(a.x, b.x),
    y0: Math.min(a.y, b.y),
    x1: Math.max(a.x, b.x),
    y1: Math.max(a.y, b.y),
  };
}

/** What a pointer-down on the canvas should start. */
export type Gesture =
  | { kind: "marquee" }
  | { kind: "move"; selectFirst: "none" | "only" | "toggle" };

/**
 * Decides a gesture from what is under the pointer.
 *
 * A grab on empty space sweeps a marquee; a grab on a node moves the selection,
 * selecting the node first if it was not part of it (so dragging always moves
 * what is under the cursor). Grabbing a node that is already selected never
 * changes the selection, so a multi-node drag stays intact.
 */
export function gestureFor(
  hit: number,
  selection: readonly number[],
  additive: boolean,
): Gesture {
  if (hit === 0) return { kind: "marquee" };
  if (selection.includes(hit)) return { kind: "move", selectFirst: "none" };
  return { kind: "move", selectFirst: additive ? "toggle" : "only" };
}

/** Whether a move gesture passed the click threshold (document units). */
export const DRAG_THRESHOLD = 0.5;

export function isDrag(dx: number, dy: number): boolean {
  return Math.hypot(dx, dy) > DRAG_THRESHOLD;
}

// ---------------------------------------------------------------------------
// Transform handles (4B)
// ---------------------------------------------------------------------------

/** Which control point of the selection box a gesture grabbed. */
export type HandleId = "nw" | "n" | "ne" | "e" | "se" | "s" | "sw" | "w" | "rotate";

/** How big a handle is drawn (screen px). */
export const HANDLE_RADIUS = 4.5;

/** How close a pointer has to be to grab a handle (screen px). */
export const HANDLE_GRAB = 9;

/** How far above the box the rotate grip sits (screen px). */
export const ROTATE_GAP = 22;

const CORNERS: readonly HandleId[] = ["nw", "ne", "se", "sw"];

/** True for the four corner handles (which scale both axes at once). */
export function isCorner(handle: HandleId): boolean {
  return CORNERS.includes(handle);
}

/** Where each handle sits on screen, given the box and the view. */
export function handlePositions(box: Box, view: View): Record<HandleId, { x: number; y: number }> {
  const nw = toScreenPoint(box.x0, box.y0, view);
  const se = toScreenPoint(box.x1, box.y1, view);
  const midX = (nw.x + se.x) / 2;
  const midY = (nw.y + se.y) / 2;
  return {
    nw,
    n: { x: midX, y: nw.y },
    ne: { x: se.x, y: nw.y },
    e: { x: se.x, y: midY },
    se,
    s: { x: midX, y: se.y },
    sw: { x: nw.x, y: se.y },
    w: { x: nw.x, y: midY },
    rotate: { x: midX, y: nw.y - ROTATE_GAP },
  };
}

/** The handle under a screen point, or `null` when the pointer is between them. */
export function hitHandle(
  at: { x: number; y: number },
  box: Box,
  view: View,
  grab = HANDLE_GRAB,
): HandleId | null {
  let best: HandleId | null = null;
  let bestDistance = grab;
  for (const [id, point] of Object.entries(handlePositions(box, view)) as Array<
    [HandleId, { x: number; y: number }]
  >) {
    const distance = Math.hypot(point.x - at.x, point.y - at.y);
    if (distance <= bestDistance) {
      best = id;
      bestDistance = distance;
    }
  }
  return best;
}

/** The point a handle drags; its opposite number is the pivot. */
function anchor(handle: HandleId, box: Box): { grab: Point; pivot: Point } {
  const left = box.x0;
  const right = box.x1;
  const top = box.y0;
  const bottom = box.y1;
  const centre: Point = { x: (left + right) / 2, y: (top + bottom) / 2 };
  switch (handle) {
    case "nw":
      return { grab: { x: left, y: top }, pivot: { x: right, y: bottom } };
    case "ne":
      return { grab: { x: right, y: top }, pivot: { x: left, y: bottom } };
    case "se":
      return { grab: { x: right, y: bottom }, pivot: { x: left, y: top } };
    case "sw":
      return { grab: { x: left, y: bottom }, pivot: { x: right, y: top } };
    case "n":
      return { grab: { x: centre.x, y: top }, pivot: { x: centre.x, y: bottom } };
    case "s":
      return { grab: { x: centre.x, y: bottom }, pivot: { x: centre.x, y: top } };
    case "w":
      return { grab: { x: left, y: centre.y }, pivot: { x: right, y: centre.y } };
    case "e":
      return { grab: { x: right, y: centre.y }, pivot: { x: left, y: centre.y } };
    case "rotate":
      return { grab: { x: centre.x, y: top }, pivot: centre };
  }
}

/** How small a factor may get before the command would be degenerate. */
const MIN_FACTOR = 0.01;

/** How far a point moved from the pivot, in multiples of the handle's span. */
function ratio(from: number, pivot: number, to: number): number | null {
  const span = from - pivot;
  if (!Number.isFinite(span) || Math.abs(span) < 1e-4) return null;
  const factor = (to - pivot) / span;
  return Number.isFinite(factor) ? factor : null;
}

/**
 * Turns a handle drag into a scale command, or `null` when the result would be
 * degenerate (the engine would refuse it, and a drag that does nothing is
 * better than a drag that throws).
 *
 * A corner scales both axes: uniformly by default (the diagonal distance ratio,
 * so the aspect never drifts) and freely with Shift. An edge handle scales one
 * axis only, which is what `SCALE_XY` exists for. The pivot is the opposite
 * corner or edge, or the box centre with `fromCenter`.
 */
export function scaleCommandFor(
  handle: HandleId,
  box: Box,
  to: Point,
  options: { uniform: boolean; fromCenter?: boolean },
): EditorCommand | null {
  if (handle === "rotate") return null;
  const { grab, pivot: opposite } = anchor(handle, box);
  const pivot = options.fromCenter
    ? { x: (box.x0 + box.x1) / 2, y: (box.y0 + box.y1) / 2 }
    : opposite;
  const horizontal = handle !== "n" && handle !== "s";
  const vertical = handle !== "e" && handle !== "w";
  const dx = horizontal ? ratio(grab.x, pivot.x, to.x) : 1;
  const dy = vertical ? ratio(grab.y, pivot.y, to.y) : 1;
  if (dx === null || dy === null) return null;
  if (options.uniform && horizontal && vertical) {
    // Uniform: the diagonal ratio, so the aspect cannot drift even when the
    // drag has no component on one axis.
    const span = Math.hypot(grab.x - pivot.x, grab.y - pivot.y);
    if (span < 1e-4) return null;
    const factor = Math.abs(Math.hypot(to.x - pivot.x, to.y - pivot.y) / span);
    if (!Number.isFinite(factor) || factor < MIN_FACTOR) return null;
    return { kind: "scale", factor, pivot };
  }
  // Free and single-axis scaling: a collapsed axis has no meaning (the engine
  // refuses it), and a drag that moved nothing is not worth a history step.
  const sx = Math.abs(dx);
  const sy = Math.abs(dy);
  if (sx < MIN_FACTOR || sy < MIN_FACTOR) return null;
  if (sx === 1 && sy === 1) return null;
  return { kind: "scaleXY", sx, sy, pivot };
}

/** Degrees a rotate drag snaps to with Shift held. */
export const ROTATE_SNAP_DEGREES = 15;

/** The angle from a pivot to a point, in degrees. */
export function angleOf(pivot: Point, at: Point): number {
  return (Math.atan2(at.y - pivot.y, at.x - pivot.x) * 180) / Math.PI;
}

/** Wraps an angle into `(-180, 180]`. */
export function wrapDegrees(degrees: number): number {
  let value = degrees % 360;
  if (value > 180) value -= 360;
  if (value <= -180) value += 360;
  return value;
}

/**
 * Turns a rotate drag into a rotate command about the box centre, or `null`
 * when the drag has not turned anything yet. With `snap` the angle lands on the
 * next `ROTATE_SNAP_DEGREES` step, which is the Shift behaviour the UI documents.
 */
export function rotateCommandFor(
  box: Box,
  from: Point,
  to: Point,
  options: { snap?: boolean } = {},
): EditorCommand | null {
  const pivot: Point = { x: (box.x0 + box.x1) / 2, y: (box.y0 + box.y1) / 2 };
  let degrees = wrapDegrees(angleOf(pivot, to) - angleOf(pivot, from));
  if (!Number.isFinite(degrees)) return null;
  if (options.snap) {
    degrees = wrapDegrees(Math.round(degrees / ROTATE_SNAP_DEGREES) * ROTATE_SNAP_DEGREES);
  }
  if (Math.abs(degrees) < 1e-3) return null;
  return { kind: "rotate", degrees: Math.fround(degrees), pivot };
}

// ---------------------------------------------------------------------------
// Local previews and the snap settings (4B)
// ---------------------------------------------------------------------------

/** An affine `[a, b, c, d, e, f]` in canvas argument order. */
export type Affine = [number, number, number, number, number, number];

/**
 * The affine a command applies, or `null` for the commands that cannot be
 * expressed as one (delete, fill, visibility, …).
 *
 * The canvas uses it to draw a gesture in progress from the geometry it already
 * has: painting never needs the module, while the engine is told the same
 * command so its own node records, bounds and snap answers agree with the screen.
 */
export function commandAffine(command: EditorCommand): Affine | null {
  switch (command.kind) {
    case "translate":
      return [1, 0, 0, 1, command.dx, command.dy];
    case "scale": {
      const { x, y } = command.pivot;
      return [command.factor, 0, 0, command.factor, x - x * command.factor, y - y * command.factor];
    }
    case "scaleXY": {
      const { x, y } = command.pivot;
      return [command.sx, 0, 0, command.sy, x - x * command.sx, y - y * command.sy];
    }
    case "rotate": {
      const radians = (command.degrees * Math.PI) / 180;
      const cos = Math.cos(radians);
      const sin = Math.sin(radians);
      const { x, y } = command.pivot;
      return [cos, sin, -sin, cos, x - x * cos + y * sin, y - x * sin - y * cos];
    }
    default:
      return null;
  }
}

/** Applies an affine to a point. */
export function applyAffine(m: Affine, point: Point): Point {
  return {
    x: m[0] * point.x + m[2] * point.y + m[4],
    y: m[1] * point.x + m[3] * point.y + m[5],
  };
}

/** The box a command moves `box` to (used to keep the handles under the drag). */
// ---------------------------------------------------------------------------
// 4C: node editing
// ---------------------------------------------------------------------------

/** How finely a cubic is sampled when hit-testing it. */
export const CURVE_SAMPLES = 32;

/** How close to a point a click has to be, in *document* units. */
export const POINT_GRAB = 6;

/** One point of a node the direct-selection tool can grab, in document space. */
export interface NodePoint {
  /** The node it belongs to. */
  node: number;
  /** What it is, for the command that moves it. */
  at: VertexAddress | HandleAddress;
  /** Where it is now, in document space. */
  to: Point;
  /** For a handle: the vertex it turns about (the lever's other end). */
  from: Point | null;
}

/** The local-space point an address refers to, or `null` when it does not exist. */
export function localPoint(node: NodeSpec, at: VertexAddress | HandleAddress): Point | null {
  const sub = node.path[at.subpath];
  if (!sub) return null;
  if (at.of === "vertex") {
    if (at.vertex === 0) return sub.start;
    const seg = sub.segs[at.vertex - 1];
    return seg ? seg.to : null;
  }
  const seg = sub.segs[at.segment];
  if (!seg || seg.kind !== KIND.CUBIC) return null;
  return at.handle === "c1" ? seg.c1 : seg.c2;
}

/**
 * Every vertex of a node, plus the control handles of its cubic segments.
 *
 * Vertices come first so that a click near both a vertex and a handle grabs the
 * vertex — the same priority the engine's own point hit-test uses.
 */
export function nodePoints(node: NodeSpec): NodePoint[] {
  const vertices: NodePoint[] = [];
  const handles: NodePoint[] = [];
  const m = node.m as Affine;
  node.path.forEach((sub, subpath) => {
    vertices.push({
      node: node.id,
      at: { of: "vertex", subpath, vertex: 0 },
      to: applyAffine(m, sub.start),
      from: null,
    });
    sub.segs.forEach((seg, index) => {
      vertices.push({
        node: node.id,
        at: { of: "vertex", subpath, vertex: index + 1 },
        to: applyAffine(m, seg.to),
        from: null,
      });
      if (seg.kind !== KIND.CUBIC) return;
      handles.push(
        {
          node: node.id,
          at: { of: "handle", subpath, segment: index, handle: "c1" },
          to: applyAffine(m, seg.c1),
          from: applyAffine(m, sub.start),
        },
        {
          node: node.id,
          at: { of: "handle", subpath, segment: index, handle: "c2" },
          to: applyAffine(m, seg.c2),
          from: applyAffine(m, seg.to),
        },
      );
    });
  });
  return [...vertices, ...handles];
}

/** The nearest point within `grab` document units, vertices first. */
export function hitPoint(
  points: readonly NodePoint[],
  at: Point,
  grab = POINT_GRAB,
): NodePoint | null {
  let best: NodePoint | null = null;
  let bestDistance = grab;
  for (const point of points) {
    const distance = Math.hypot(point.to.x - at.x, point.to.y - at.y);
    if (distance <= bestDistance) {
      best = point;
      bestDistance = distance;
    }
  }
  return best;
}

/**
 * The place on a node's outline nearest to `at`, or `null`.
 *
 * Cubics are sampled rather than solved: the answer is a parameter for
 * `insertPoint`, and landing within a sample of where the user clicked is what
 * the gesture means. The sample index is divided by [`CURVE_SAMPLES`], so the
 * inserted vertex stays strictly inside the segment.
 */
export function hitSegment(
  node: NodeSpec,
  at: Point,
  grab = POINT_GRAB,
): SegmentAddress | null {
  const m = node.m as Affine;
  let best: SegmentAddress | null = null;
  let bestDistance = grab;
  // A straight segment is measured exactly (a projection needs no samples); a
  // cubic is walked in steps, which is also how the engine flattens one.
  const consider = (subpath: number, segment: number, t: number, at0: Point): void => {
    const point = applyAffine(m, at0);
    const distance = Math.hypot(point.x - at.x, point.y - at.y);
    if (distance > bestDistance) return;
    // The parameter stays strictly inside the segment: `insertPoint` refuses
    // `t = 0` and `t = 1`, which are the segment's own ends.
    best = { of: "segment", subpath, segment, t: Math.min(0.95, Math.max(0.05, t)) };
    bestDistance = distance;
  };
  node.path.forEach((sub, subpath) => {
    let start = sub.start;
    sub.segs.forEach((seg, segment) => {
      if (seg.kind === KIND.LINE) {
        const dx = seg.to.x - start.x;
        const dy = seg.to.y - start.y;
        const length2 = dx * dx + dy * dy;
        // The projection has to happen in the path's own space, so the pointer
        // is pulled back through the node's transform first.
        const local = invertAffine(m, at) ?? at;
        const t =
          length2 > 0
            ? Math.min(1, Math.max(0, ((local.x - start.x) * dx + (local.y - start.y) * dy) / length2))
            : 0;
        consider(subpath, segment, t, {
          x: start.x + dx * t,
          y: start.y + dy * t,
        });
      } else {
        const c1 = seg.c1;
        const c2 = seg.c2;
        for (let i = 0; i <= CURVE_SAMPLES; i++) {
          const t = i / CURVE_SAMPLES;
          consider(subpath, segment, t, cubicAt(start, c1, c2, seg.to, t));
        }
      }
      start = seg.to;
    });
  });
  return best;
}

/** A cubic Bézier evaluated at `t`, in the path's own space. */
export function cubicAt(start: Point, c1: Point, c2: Point, to: Point, t: number): Point {
  const u = 1 - t;
  const a = u * u * u;
  const b = 3 * u * u * t;
  const c = 3 * u * t * t;
  const d = t * t * t;
  return {
    x: a * start.x + b * c1.x + c * c2.x + d * to.x,
    y: a * start.y + b * c1.y + c * c2.y + d * to.y,
  };
}

/**
 * A local subpath with one point moved — the live outline a drag draws.
 *
 * The engine is not asked until the gesture ends (one gesture, one command, one
 * undo step), so the drag has to paint its own geometry. Moving a vertex drags
 * its attached handles with it, exactly as the engine's own `move_vertex` does.
 */
export function subpathWithPointMoved(
  sub: Subpath,
  at: VertexAddress | HandleAddress,
  to: Point,
): Subpath {
  if (at.of === "handle") {
    const segs = sub.segs.map((seg, index) => {
      if (index !== at.segment || seg.kind !== KIND.CUBIC) return seg;
      return at.handle === "c1" ? { ...seg, c1: to } : { ...seg, c2: to };
    });
    return { ...sub, segs };
  }
  // Exactly what the engine's `move_vertex` does: the anchor moves, then its
  // two handles follow by the same delta — the incoming one belongs to the
  // segment that *ends* here, the outgoing one to the segment that starts here.
  // At vertex 0 the incoming edge is the implicit straight close, so there is
  // no handle to drag, and the last vertex's outgoing edge is that same close.
  if (at.vertex === 0) {
    const delta = { x: to.x - sub.start.x, y: to.y - sub.start.y };
    const first = sub.segs[0];
    const segs =
      first && first.kind === KIND.CUBIC
        ? [{ ...first, c1: { x: first.c1.x + delta.x, y: first.c1.y + delta.y } }, ...sub.segs.slice(1)]
        : sub.segs;
    return { ...sub, start: to, segs };
  }
  const index = at.vertex - 1;
  const anchor = sub.segs[index]?.to;
  if (!anchor) return sub; // no such vertex: nothing to move
  const delta = { x: to.x - anchor.x, y: to.y - anchor.y };
  const segs = sub.segs.map((seg, i) => {
    if (i === index) {
      // The segment that ends at the moved vertex: its end moves and so does
      // the handle that hangs off that end.
      return seg.kind === KIND.CUBIC
        ? { ...seg, c2: { x: seg.c2.x + delta.x, y: seg.c2.y + delta.y }, to }
        : { ...seg, to };
    }
    if (i === index + 1 && seg.kind === KIND.CUBIC) {
      // The segment that leaves the moved vertex: its handle follows too.
      return { ...seg, c1: { x: seg.c1.x + delta.x, y: seg.c1.y + delta.y } };
    }
    return seg;
  });
  return { ...sub, segs };
}

/** The whole node with one point moved (the outline a point drag paints). */
export function nodeWithPointMoved(
  node: NodeSpec,
  at: VertexAddress | HandleAddress,
  to: Point,
): NodeSpec {
  // `to` arrives in document space; the path is local, so the point is inverted
  // through the node's own transform before it is written into the geometry.
  const local = invertAffine(node.m as Affine, to) ?? to;
  return {
    ...node,
    path: node.path.map((sub, index) =>
      index === at.subpath ? subpathWithPointMoved(sub, at, local) : sub,
    ),
  };
}

/** The inverse of an affine, or `null` when it is singular. */
export function invertAffine(m: Affine, point: Point): Point | null {
  const [a, b, c, d, e, f] = m;
  const det = a * d - b * c;
  if (Math.abs(det) < 1e-9) return null;
  const x = point.x - e;
  const y = point.y - f;
  return { x: (d * x - c * y) / det, y: (a * y - b * x) / det };
}

export function boxAfterCommand(box: Box, command: EditorCommand): Box {
  const m = commandAffine(command);
  if (!m) return box;
  const corners = [
    applyAffine(m, { x: box.x0, y: box.y0 }),
    applyAffine(m, { x: box.x1, y: box.y0 }),
    applyAffine(m, { x: box.x1, y: box.y1 }),
    applyAffine(m, { x: box.x0, y: box.y1 }),
  ];
  return {
    x0: Math.min(...corners.map((c) => c.x)),
    y0: Math.min(...corners.map((c) => c.y)),
    x1: Math.max(...corners.map((c) => c.x)),
    y1: Math.max(...corners.map((c) => c.y)),
  };
}

/** Which snapping families the user has switched on, and how close is close. */
export interface SnapSettings {
  /** Snap to the canvas edges and centre lines. */
  canvas: boolean;
  /** Snap to other nodes' edges and centres. */
  nodes: boolean;
  /** Snap to a grid. */
  grid: boolean;
  /** Grid pitch in document units. */
  gridStep: number;
  /** How far a target can be and still attract, in document units. */
  tolerance: number;
}

/** Sensible starting settings: canvas and neighbours on, grid off. */
export const SNAP_DEFAULTS: SnapSettings = {
  canvas: true,
  nodes: true,
  grid: false,
  gridStep: 8,
  tolerance: 6,
};

/** The flag mask the ABI wants for these settings. */
export function snapFlags(settings: SnapSettings): number {
  return (
    (settings.canvas ? SNAP.CANVAS : 0) |
    (settings.nodes ? SNAP.NODES : 0) |
    (settings.grid ? SNAP.GRID : 0)
  );
}

/** The label for one guide kind (the status line and the overlay). */
export const GUIDE_TEXT: Record<number, string> = {
  [GUIDE_KIND.CANVAS_EDGE]: "canvas edge",
  [GUIDE_KIND.CANVAS_CENTER]: "canvas centre",
  [GUIDE_KIND.NODE_EDGE]: "node edge",
  [GUIDE_KIND.NODE_CENTER]: "node centre",
  [GUIDE_KIND.GRID]: "grid",
};
