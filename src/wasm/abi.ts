/**
 * The Icon Forge editor ABI, mirrored from `crates/isg-wasm/src/{abi,doc_blob}.rs`.
 *
 * Everything here is pure: word tables in, word tables out, no WebAssembly
 * instance involved. That keeps the byte-level contract testable in vitest and
 * keeps `editor.ts` to the one thing it must do — drive the instance.
 *
 * The format is deliberately spelled out on both sides (Rust and TypeScript) and
 * *checked* on both sides: `crates/isg-wasm/tests/abi.rs` round-trips the blob
 * and `src/wasm/abi.test.ts` does the same from here, so a silent drift shows up
 * as a failing test rather than as mangled geometry on screen.
 */

/** ABI level of the exported entry points; must match `abi::ABI_VERSION`. */
export const ABI_VERSION = 2;

/** Words in the input table. */
export const IN_WORDS = 1 << 20;

/** Words in the output table. */
export const OUT_WORDS = 1 << 18;

/** Largest document the module will load. */
export const MAX_NODES = 4096;

/** Words in a document header: width, height, node count, total. */
export const DOC_HEADER = 4;

/** Words in a node record header; its path blob follows. */
export const NODE_RECORD_HEADER = 11;

/** Slot (within the node record header) holding the node's group id, 0 = none. */
export const NODE_GROUP_SLOT = 9;

/** Slot (within the node record header) holding the path blob's word count. */
export const NODE_PATH_SLOT = 10;

/** Words in a subpath header: start x, start y, closed, segment count. */
export const SUBPATH_HEADER = 4;

/** Words per segment record: kind, c1, c2, to. */
export const SEGMENT_WORDS = 7;

/** Feature numbers — see `abi::feature`. */
export const FEATURE = {
  VERSION: 0,
  DOC_LOAD: 1,
  NODE_COUNT: 2,
  REVISION: 3,
  NODE_SYNC: 4,
  NODE_BOUNDS: 5,
  PATH_FLUSH: 6,
  SELECTION_COUNT: 7,
  SELECTION_AT: 8,
  SELECTION_IDS: 9,
  SELECTION_BOUNDS: 10,
  SELECT_ALL: 11,
  SELECT_CLEAR: 12,
  SELECT_ONLY: 13,
  SELECT_ADD: 14,
  SELECT_TOGGLE: 15,
  PICK: 16,
  MARQUEE: 17,
  GET_TOLERANCE: 18,
  SET_TOLERANCE: 19,
  APPLY_SPEC: 20,
  UNDO: 21,
  REDO: 22,
  CAN_UNDO: 23,
  CAN_REDO: 24,
  HISTORY_LEN: 25,
  HISTORY_CURSOR: 26,
  HISTORY_REDO_DEPTH: 27,
  HISTORY_DROPPED: 28,
  UNDO_LABEL: 29,
  REDO_LABEL: 30,
  LAST_LABEL: 31,
  ERROR: 32,
  DOC_SIZE: 33,
  SEGMENT_COUNT: 34,
  NODE_AT: 35,
  CLOSE: 36,
  SNAP: 37,
  PREVIEW: 38,
  SVG_NODES: 39,
  IMPORT_SVG: 40,
} as const;

/** Command spec opcodes — see `abi::OP_*`. */
export const OP = {
  TRANSLATE: 1,
  SCALE: 2,
  ROTATE: 3,
  CENTER: 4,
  SET_FILL: 5,
  SET_VISIBLE: 6,
  REORDER: 7,
  DUPLICATE: 8,
  DELETE: 9,
  GROUP: 10,
  UNGROUP: 11,
  ARRANGE: 12,
  ALIGN: 13,
  SCALE_XY: 14,
  MOVE_POINT: 15,
  MOVE_HANDLE: 16,
  INSERT_POINT: 17,
  DELETE_POINT: 18,
  SET_SEGMENT: 19,
  BOOLEAN: 20,
} as const;

/** `SNAP` input flags: which target families snapping may use. */
export const SNAP = {
  CANVAS: 1 << 0,
  NODES: 1 << 1,
  GRID: 1 << 2,
} as const;

/** Every `SNAP` flag the ABI defines (a bit outside this mask is refused). */
export const SNAP_FLAGS = SNAP.CANVAS | SNAP.NODES | SNAP.GRID;

/** `PREVIEW` argument `a`: install a preview (`b` = spec offset) or drop it. */
export const PREVIEW = { SET: 0, CLEAR: 1 } as const;

/** Words per snap guide record: axis, kind, position, from, to. */
export const GUIDE_WORDS = 5;

/** Most guides one `SNAP` answer carries (mirrors `editor::snap::MAX_GUIDES`). */
export const MAX_SNAP_GUIDES = 8;

/** Which line a guide describes; raw values are wire values (see `GuideKind`). */
export const GUIDE_KIND = {
  CANVAS_EDGE: 0,
  CANVAS_CENTER: 1,
  NODE_EDGE: 2,
  NODE_CENTER: 3,
  GRID: 4,
} as const;

/** Where an arranged node ends up. */
export type ArrangeTo = "front" | "back";

/** What an alignment lines the selection up against. */
export type AlignFrame = "selection" | "canvas";

/** Which reference line of the frame an alignment uses. */
export type AlignEdge = "left" | "hcenter" | "right" | "top" | "vcenter" | "bottom";

/** Wire values for `ArrangeTo` (see `isg_core::editor::ArrangeTo::raw`). */
export const ARRANGE_RAW: Record<ArrangeTo, number> = { front: 0, back: 1 };

/** Wire values for `AlignFrame`. */
export const ALIGN_FRAME_RAW: Record<AlignFrame, number> = { selection: 0, canvas: 1 };

/** Wire values for `AlignEdge`. */
export const ALIGN_EDGE_RAW: Record<AlignEdge, number> = {
  left: 0,
  hcenter: 1,
  right: 2,
  top: 3,
  vcenter: 4,
  bottom: 5,
};

/** The edges an align panel offers, in the order the ABI numbers them. */
export const ALIGN_EDGES: readonly AlignEdge[] = [
  "left",
  "hcenter",
  "right",
  "top",
  "vcenter",
  "bottom",
] as const;

/** Error codes — see `abi::ERR_*`. */
export const ERR = {
  NONE: 0,
  BAD_FEATURE: 1,
  NO_DOCUMENT: 2,
  BAD_ARGUMENT: 3,
  CAPACITY: 4,
  NO_HISTORY: 5,
  NO_SELECTION: 6,
  DEGENERATE: 7,
  TRANSPARENT: 8,
  NO_OP: 9,
  MISSING_NODE: 10,
  MALFORMED_SVG: 11,
} as const;

/** Human-readable text for an error code (status line, dev console). */
export const ERR_TEXT: Record<number, string> = {
  [ERR.BAD_FEATURE]: "unknown editor feature",
  [ERR.NO_DOCUMENT]: "no document is loaded",
  [ERR.BAD_ARGUMENT]: "bad argument",
  [ERR.CAPACITY]: "result does not fit the editor buffer",
  [ERR.NO_HISTORY]: "nothing to undo or redo",
  [ERR.NO_SELECTION]: "select something first",
  [ERR.DEGENERATE]: "that transform is degenerate",
  [ERR.TRANSPARENT]: "a fully transparent fill would be invisible",
  [ERR.NO_OP]: "nothing would change",
  [ERR.MISSING_NODE]: "that node no longer exists",
  [ERR.MALFORMED_SVG]: "that file is not readable svg",
};

/** Segment kinds. */
export const KIND = { LINE: 0, CUBIC: 1 } as const;

/** The four pathfinder operations (4C). */
export type BooleanOp = "union" | "subtract" | "intersect" | "exclude";

/** Raw wire values for [`BooleanOp`] — see `BooleanOp::raw` in `isg-core`. */
export const BOOLEAN_RAW: Record<BooleanOp, number> = {
  union: 0,
  subtract: 1,
  intersect: 2,
  exclude: 3,
};

/** The four operations in the order the UI offers them. */
export const BOOLEAN_OPS: readonly BooleanOp[] = ["union", "subtract", "intersect", "exclude"];

/** A segment's shape, as `setSegment` names it. */
export type SegmentKind = "line" | "cubic";

/** Raw wire values for [`SegmentKind`]. */
export const SEGMENT_KIND_RAW: Record<SegmentKind, number> = { line: KIND.LINE, cubic: KIND.CUBIC };

/** Which of a cubic's two control handles is being dragged. */
export type HandleSlot = "c1" | "c2";

/** Raw wire values for [`HandleSlot`]. */
export const HANDLE_RAW: Record<HandleSlot, number> = { c1: 0, c2: 1 };

/** A vertex of a subpath: `0` is the start, `k` the end of segment `k - 1`. */
export interface VertexAddress {
  of: "vertex";
  subpath: number;
  vertex: number;
}

/** One of a cubic segment's two control handles. */
export interface HandleAddress {
  of: "handle";
  subpath: number;
  segment: number;
  handle: HandleSlot;
}

/** A place along a segment, at parameter `t` in `(0, 1)`. */
export interface SegmentAddress {
  of: "segment";
  subpath: number;
  segment: number;
  t: number;
}

/**
 * A point a node edit can address: a vertex, a control handle or a place on a
 * segment (for `insertPoint`). Every 4C command carries one.
 */
export type PointAddress = VertexAddress | HandleAddress | SegmentAddress;


/** RGBA, each channel 0–255. */
export type Rgba = [number, number, number, number];

/** A point in document space. */
export interface Point {
  x: number;
  y: number;
}

/** One path segment; the subpath's start is the previous segment's end. */
export type Segment =
  | { kind: typeof KIND.LINE; to: Point }
  | { kind: typeof KIND.CUBIC; c1: Point; c2: Point; to: Point };

/** A subpath: a start point, its segments, and whether it closes. */
export interface Subpath {
  start: Point;
  closed: boolean;
  segs: Segment[];
}

/** One node as the loader hands it in (a `Doc` node in `isg-core`). */
export interface NodeSpec {
  id: number;
  /** Affine `[a, b, c, d, e, f]`, canvas argument order. */
  m: number[];
  fill: Rgba;
  visible: boolean;
  /** Group id, `0` or absent when the node is not in a group. */
  group?: number;
  path: Subpath[];
}

/** One snap guide: a line the editor drew to explain a correction. */
export interface SnapGuide {
  /** `0` = a vertical line at `position` in x, `1` = a horizontal line in y. */
  axis: number;
  /** `GUIDE_KIND.*`: what the line came from. */
  kind: number;
  /** The line's coordinate on its own axis. */
  position: number;
  /** Where the line starts on the other axis. */
  from: number;
  /** Where the line ends on the other axis. */
  to: number;
}

/** The answer to one `SNAP` request. */
export interface SnapResult {
  /** The corrected delta the UI should actually move by. */
  dx: number;
  dy: number;
  guides: SnapGuide[];
}

/** A box: min x, min y, max x, max y. */
export interface Box {
  x0: number;
  y0: number;
  x1: number;
  y1: number;
}

/** The commands the UI can produce, mirroring `isg_core::editor::Command`. */
export type EditorCommand =
  | { kind: "translate"; dx: number; dy: number }
  | { kind: "scale"; factor: number; pivot: Point }
  | { kind: "rotate"; degrees: number; pivot: Point }
  | { kind: "center" }
  | { kind: "fill"; rgba: Rgba }
  | { kind: "visible"; to: boolean }
  | { kind: "reorder"; up: boolean }
  | { kind: "duplicate"; dx: number; dy: number }
  | { kind: "delete" }
  | { kind: "scaleXY"; sx: number; sy: number; pivot: Point }
  | { kind: "arrange"; to: ArrangeTo }
  | { kind: "align"; frame: AlignFrame; edge: AlignEdge }
  | { kind: "group" }
  | { kind: "ungroup" }
  // 4C: node editing and booleans.
  | { kind: "movePoint"; node: number; at: VertexAddress; to: Point }
  | { kind: "moveHandle"; node: number; at: HandleAddress; to: Point }
  | { kind: "insertPoint"; node: number; at: SegmentAddress }
  | { kind: "deletePoint"; node: number; at: VertexAddress }
  | { kind: "setSegment"; node: number; at: SegmentAddress; to: SegmentKind }
  | { kind: "boolean"; op: BooleanOp };

/** Packs an `f32` into its bit pattern. */
export function f2w(value: number): number {
  const buffer = new ArrayBuffer(4);
  new DataView(buffer).setFloat32(0, value, true);
  return new Uint32Array(buffer)[0];
}

/** Unpacks an `f32` bit pattern. */
export function w2f(word: number): number {
  const buffer = new ArrayBuffer(4);
  new Uint32Array(buffer)[0] = word;
  return new DataView(buffer).getFloat32(0, true);
}

/** Packs an RGBA fill into the `0xRRGGBBAA` word the ABI uses. */
export function packRgba(rgba: Rgba): number {
  return (
    (((rgba[0] & 0xff) << 24) |
      ((rgba[1] & 0xff) << 16) |
      ((rgba[2] & 0xff) << 8) |
      (rgba[3] & 0xff)) >>>
    0
  );
}

/** Unpacks a `0xRRGGBBAA` word. */
export function unpackRgba(word: number): Rgba {
  return [(word >>> 24) & 0xff, (word >>> 16) & 0xff, (word >>> 8) & 0xff, word & 0xff];
}

function pushPoint(out: number[], point: Point): void {
  out.push(f2w(point.x), f2w(point.y));
}

/** Encodes a path as a path blob (used inside node records and by `PATH_FLUSH`). */
export function encodePath(path: Subpath[]): number[] {
  const out: number[] = [path.length];
  for (const sub of path) {
    pushPoint(out, sub.start);
    out.push(sub.closed ? 1 : 0, sub.segs.length);
    for (const seg of sub.segs) {
      if (seg.kind === KIND.LINE) {
        out.push(KIND.LINE);
        pushPoint(out, seg.to);
        pushPoint(out, seg.to);
        pushPoint(out, seg.to);
      } else {
        out.push(KIND.CUBIC);
        pushPoint(out, seg.c1);
        pushPoint(out, seg.c2);
        pushPoint(out, seg.to);
      }
    }
  }
  return out;
}

/**
 * Encodes a whole document: the blob `DOC_LOAD` expects.
 *
 * @throws RangeError when the document is not loadable (no canvas size, more
 * nodes than the module accepts, or a non-finite coordinate) — the module would
 * reject it anyway, and failing here names the offending value.
 */
export function encodeDoc(width: number, height: number, nodes: NodeSpec[]): Uint32Array {
  if (!Number.isFinite(width) || !Number.isFinite(height) || width <= 0 || height <= 0) {
    throw new RangeError(`a document needs a positive canvas size, got ${width}×${height}`);
  }
  if (nodes.length > MAX_NODES) {
    throw new RangeError(`${nodes.length} nodes exceeds the editor's ${MAX_NODES}`);
  }
  const out: number[] = [f2w(width), f2w(height), nodes.length, 0];
  for (const node of nodes) {
    if (node.id <= 0) throw new RangeError(`node ids start at 1, got ${node.id}`);
    if (!node.path.every((sub) => Number.isFinite(sub.start.x) && Number.isFinite(sub.start.y))) {
      throw new RangeError(`node ${node.id} has a non-finite start point`);
    }
    const path = encodePath(node.path);
    out.push(
      node.id,
      ...node.m.map(f2w),
      packRgba(node.fill),
      node.visible ? 1 : 0,
      node.group ?? 0,
      path.length,
    );
    out.push(...path);
  }
  out[3] = out.length;
  return Uint32Array.from(out);
}

/** Decodes one path blob starting at `at`; returns the path and the words read. */
export function decodePath(
  words: Uint32Array,
  at: number,
): { path: Subpath[]; words: number } {
  let cursor = at;
  const count = words[cursor++];
  const path: Subpath[] = [];
  for (let i = 0; i < count; i++) {
    const start = { x: w2f(words[cursor]), y: w2f(words[cursor + 1]) };
    const closed = words[cursor + 2] !== 0;
    const segCount = words[cursor + 3];
    cursor += SUBPATH_HEADER;
    const segs: Segment[] = [];
    for (let s = 0; s < segCount; s++) {
      const kind = words[cursor];
      const point = (slot: number): Point => ({
        x: w2f(words[cursor + 1 + slot * 2]),
        y: w2f(words[cursor + 2 + slot * 2]),
      });
      if (kind === KIND.LINE) {
        segs.push({ kind: KIND.LINE, to: point(2) });
      } else if (kind === KIND.CUBIC) {
        segs.push({ kind: KIND.CUBIC, c1: point(0), c2: point(1), to: point(2) });
      } else {
        throw new RangeError(`unknown segment kind ${kind} at word ${cursor}`);
      }
      cursor += SEGMENT_WORDS;
    }
    path.push({ start, closed, segs });
  }
  return { path, words: cursor - at };
}

/**
 * Walks `count` node records written by `NODE_SYNC`.
 *
 * Records are self-describing (each header carries its path length) and the
 * module zeroes the word after the last one, so a walk stops at the terminator
 * even if the output table still holds words from an earlier call.
 */
export function decodeNodeRecords(
  words: Uint32Array,
  offset: number,
  count: number,
): NodeSpec[] {
  const nodes: NodeSpec[] = [];
  let at = offset;
  for (let i = 0; i < count; i++) {
    const id = words[at];
    if (id === 0) break; // terminator: ids are never 0
    const m = Array.from({ length: 6 }, (_, k) => w2f(words[at + 1 + k]));
    const fill = unpackRgba(words[at + 7]);
    const visible = words[at + 8] !== 0;
    const group = words[at + NODE_GROUP_SLOT];
    const pathWords = words[at + NODE_PATH_SLOT];
    const { path, words: read } = decodePath(words, at + NODE_RECORD_HEADER);
    if (read !== pathWords) {
      throw new RangeError(`node ${id} declares ${pathWords} path words but carries ${read}`);
    }
    nodes.push({ id, m, fill, visible, group, path });
    at += NODE_RECORD_HEADER + pathWords;
  }
  return nodes;
}

/** Encodes a command as the spec `APPLY_SPEC` expects. */
export function encodeCommand(command: EditorCommand): Uint32Array {
  switch (command.kind) {
    case "translate":
      return Uint32Array.from([OP.TRANSLATE, f2w(command.dx), f2w(command.dy)]);
    case "scale":
      return Uint32Array.from([
        OP.SCALE,
        f2w(command.factor),
        f2w(command.pivot.x),
        f2w(command.pivot.y),
      ]);
    case "rotate":
      return Uint32Array.from([
        OP.ROTATE,
        f2w(command.degrees),
        f2w(command.pivot.x),
        f2w(command.pivot.y),
      ]);
    case "center":
      return Uint32Array.from([OP.CENTER]);
    case "fill":
      return Uint32Array.from([OP.SET_FILL, packRgba(command.rgba)]);
    case "visible":
      return Uint32Array.from([OP.SET_VISIBLE, command.to ? 1 : 0]);
    case "reorder":
      return Uint32Array.from([OP.REORDER, command.up ? 1 : 0]);
    case "duplicate":
      return Uint32Array.from([OP.DUPLICATE, f2w(command.dx), f2w(command.dy)]);
    case "delete":
      return Uint32Array.from([OP.DELETE]);
    case "scaleXY":
      return Uint32Array.from([
        OP.SCALE_XY,
        f2w(command.sx),
        f2w(command.sy),
        f2w(command.pivot.x),
        f2w(command.pivot.y),
      ]);
    case "arrange":
      return Uint32Array.from([OP.ARRANGE, ARRANGE_RAW[command.to]]);
    case "align":
      return Uint32Array.from([
        OP.ALIGN,
        ALIGN_FRAME_RAW[command.frame],
        ALIGN_EDGE_RAW[command.edge],
      ]);
    case "group":
      return Uint32Array.from([OP.GROUP]);
    case "ungroup":
      return Uint32Array.from([OP.UNGROUP]);
    case "movePoint":
      return Uint32Array.from([
        OP.MOVE_POINT,
        command.node,
        command.at.subpath,
        command.at.vertex,
        f2w(command.to.x),
        f2w(command.to.y),
      ]);
    case "moveHandle":
      return Uint32Array.from([
        OP.MOVE_HANDLE,
        command.node,
        command.at.subpath,
        command.at.segment,
        HANDLE_RAW[command.at.handle],
        f2w(command.to.x),
        f2w(command.to.y),
      ]);
    case "insertPoint":
      return Uint32Array.from([
        OP.INSERT_POINT,
        command.node,
        command.at.subpath,
        command.at.segment,
        f2w(command.at.t),
      ]);
    case "deletePoint":
      return Uint32Array.from([
        OP.DELETE_POINT,
        command.node,
        command.at.subpath,
        command.at.vertex,
      ]);
    case "setSegment":
      return Uint32Array.from([
        OP.SET_SEGMENT,
        command.node,
        command.at.subpath,
        command.at.segment,
        SEGMENT_KIND_RAW[command.to],
      ]);
    case "boolean":
      return Uint32Array.from([OP.BOOLEAN, BOOLEAN_RAW[command.op]]);
  }
}

/**
 * Encodes a `SNAP` request.
 *
 * `tolerance` is in document units; `flags` is a mask of the `SNAP.*` families
 * the user has switched on. With none of them set the module hands the delta
 * back unchanged — that is "snapping off", not an error.
 */
export function encodeSnapRequest(
  dx: number,
  dy: number,
  tolerance: number,
  flags: number,
  gridStep: number,
): Uint32Array {
  return Uint32Array.from([f2w(dx), f2w(dy), f2w(tolerance), flags, f2w(gridStep)]);
}

/**
 * Reads a `SNAP` answer out of the output table.
 *
 * The module writes `[dx, dy, guide count, guide records…, 0]` and terminates
 * the list, so the walk is driven by the count and the terminator is checked
 * rather than trusted.
 */
export function decodeSnapResult(words: Uint32Array): SnapResult {
  const dx = w2f(words[0]);
  const dy = w2f(words[1]);
  const count = Math.min(words[2], MAX_SNAP_GUIDES);
  const guides: SnapGuide[] = [];
  for (let i = 0; i < count; i++) {
    const at = 3 + i * GUIDE_WORDS;
    guides.push({
      axis: words[at],
      kind: words[at + 1],
      position: w2f(words[at + 2]),
      from: w2f(words[at + 3]),
      to: w2f(words[at + 4]),
    });
  }
  if (words[3 + count * GUIDE_WORDS] !== 0) {
    throw new RangeError("the snap guide list is not terminated");
  }
  return { dx, dy, guides };
}

/** Words a `SNAP` answer can occupy at most (header + guides + terminator). */
export const SNAP_ANSWER_WORDS = 3 + MAX_SNAP_GUIDES * GUIDE_WORDS + 1;

/** Words in the SVG spec the `SVG_NODES` / `IMPORT_SVG` features read. */
export const SVG_SPEC_WORDS = 9;

/**
 * Encodes the spec both SVG features take.
 *
 * The header is the first node id (used only when parsing), the placement, the
 * default fill and the byte length of the text, which follows packed four bytes
 * to a word — the same packing `decodeText` reads back.
 */
export function encodeSvgSpec(
  text: string,
  firstId: number,
  placement: readonly number[],
  fill: Rgba,
): number[] {
  const bytes = new TextEncoder().encode(text);
  const words = [firstId, ...placement.map(f2w), packRgba(fill), bytes.length];
  for (let i = 0; i < bytes.length; i += 4) {
    let word = 0;
    for (let j = 0; j < 4 && i + j < bytes.length; j++) word |= bytes[i + j] << (8 * j);
    words.push(word >>> 0);
  }
  return words;
}

/** Unpacks UTF-8 text from `count` words of the output table. */
export function decodeText(words: Uint32Array, bytes: number): string {
  const out = new Uint8Array(bytes);
  for (let i = 0; i < bytes; i++) {
    out[i] = (words[i >> 2] >>> ((i & 3) * 8)) & 0xff;
  }
  return new TextDecoder().decode(out);
}
