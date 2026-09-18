/**
 * `EditorSession` — the TypeScript half of the editor ABI.
 *
 * One instance wraps one WebAssembly module instance. Interactive work (hit
 * testing, dragging, undo) runs entirely inside it, which is the point of
 * Phase 4: ARCHITECTURE §2 forbids per-frame work from crossing the IPC boundary.
 *
 * ## Three traps this file exists to hide
 *
 * 1. **Fresh views.** `WebAssembly.Memory.grow` *detaches* the previous
 *    `ArrayBuffer`, so a cached `Uint32Array` silently starts reading zeros after
 *    any call that allocates. `#view()` re-derives the view on every use and
 *    nothing is cached across a call.
 * 2. **A write-once error channel.** The module answers `0` on failure and keeps
 *    the reason in a separate slot — and that slot reports the *previous* call.
 *    `call()` therefore reads it immediately and caches it, so a later
 *    bookkeeping call (like `REVISION`) cannot erase the diagnosis.
 * 3. **Record walks, not copies.** `NODE_SYNC` returns a *record* count and each
 *    record is self-describing (its path length is in its header) with a zero
 *    terminator after the last one, so reading the output region as a view is
 *    both cheaper than copying and immune to stale words from earlier calls.
 */

import {
  ABI_VERSION,
  decodeNodeRecords,
  decodePath,
  decodeSnapResult,
  decodeText,
  encodeCommand,
  encodeDoc,
  encodeSnapRequest,
  encodeSvgSpec,
  ERR,
  ERR_TEXT,
  FEATURE,
  f2w,
  KIND,
  NODE_GROUP_SLOT,
  NODE_PATH_SLOT,
  NODE_RECORD_HEADER,
  PREVIEW,
  SEGMENT_WORDS,
  SNAP_ANSWER_WORDS,
  SUBPATH_HEADER,
  unpackRgba,
  w2f,
  type Box,
  type EditorCommand,
  type NodeSpec,
  type Rgba,
  type Segment,
  type SnapResult,
  type Subpath,
} from "./abi";

/** Which target families a snap request may use, and how close counts. */
export interface SnapQuery {
  /** Distance in document units within which a target attracts. */
  tolerance: number;
  /** Mask of `SNAP.*` flags; `0` means snapping is off and the delta passes through. */
  flags: number;
  /** Grid pitch in document units (ignored unless `SNAP.GRID` is set). */
  gridStep: number;
}

/**
 * A live view over module memory.
 *
 * Spelled with the generic argument because the DOM and Node typings disagree
 * about whether `WebAssembly.Memory.buffer` can be shared — the view is the same
 * either way, and this keeps both type checkers happy.
 */
export type WordView = Uint32Array<ArrayBufferLike>;
/** A live float view over module memory. */
export type FloatView = Float32Array<ArrayBufferLike>;

/** A segment as seen while walking the buffer (valid only inside the callback). */
export interface SegmentCursor {
  /** `KIND.LINE` or `KIND.CUBIC`; the `to` point is at the same slots either way. */
  kind: number;
  c1x: number;
  c1y: number;
  c2x: number;
  c2y: number;
  tox: number;
  toy: number;
}

/** A subpath as seen while walking the buffer (valid only inside the callback). */
export interface SubpathCursor {
  startX: number;
  startY: number;
  closed: boolean;
  /** Word offset of this subpath's first segment (advanced reading). */
  segBase: number;
  /** Segments in this subpath (advanced reading). */
  segments: number;
  /** Live word view of module memory (advanced reading). */
  words: Uint32Array;
  /** Live float view over the same words (advanced reading). */
  floats: Float32Array;
  /** Runs the visitor for each segment of this subpath. */
  eachSegment: (visit: (segment: SegmentCursor) => void) => number;
}

/** A node as seen while walking the buffer (valid only inside the callback). */
export interface NodeCursor {
  id: number;
  /** The node's affine as a live view of 6 floats, canvas argument order. */
  m: FloatView;
  /** Word offset of this node's path blob (advanced, allocation-free reading). */
  pathAt: number;
  /** Words in this node's path blob, including its subpath count. */
  pathWords: number;
  /** Live word view of module memory (advanced reading). */
  words: Uint32Array;
  /** Live float view over the same words (advanced reading). */
  floats: Float32Array;
  /** Packed `0xRRGGBBAA`. */
  fill: number;
  visible: boolean;
  /** Group id, `0` when the node is not in a group. */
  group: number;
  /** Runs the visitor for each subpath of this node. */
  eachSubpath: (visit: (subpath: SubpathCursor) => void) => number;
}

/** One node cursor, reused for every node (see `forEachNode`). */
const NODE_CURSOR: NodeCursor = {
  id: 0,
  m: new Float32Array(6),
  fill: 0,
  visible: true,
  group: 0,
  pathAt: 0,
  pathWords: 0,
  words: new Uint32Array(0),
  floats: new Float32Array(0),
  eachSubpath(visit: (subpath: SubpathCursor) => void): number {
    // The path blob opens with its subpath count, which the header's word count
    // covers too, so the walk starts one word in.
    const words = NODE_CURSOR.words;
    const floats = NODE_CURSOR.floats;
    const end = NODE_CURSOR.pathAt + NODE_CURSOR.pathWords;
    let at = NODE_CURSOR.pathAt + 1;
    let visited = 0;
    while (at + SUBPATH_HEADER <= end) {
      CURRENT_SUBPATH.startX = floats[at];
      CURRENT_SUBPATH.startY = floats[at + 1];
      CURRENT_SUBPATH.closed = words[at + 2] !== 0;
      CURRENT_SUBPATH.segBase = at + SUBPATH_HEADER;
      CURRENT_SUBPATH.segments = words[at + 3];
      CURRENT_SUBPATH.words = words;
      CURRENT_SUBPATH.floats = floats;
      visit(CURRENT_SUBPATH);
      at = CURRENT_SUBPATH.segBase + CURRENT_SUBPATH.segments * SEGMENT_WORDS;
      visited += 1;
    }
    return visited;
  },
};

/**
 * One subpath cursor, reused for every subpath.
 *
 * A segment record is `[kind, c1x, c1y, c2x, c2y, tox, toy]`: a cubic uses every
 * slot and a line repeats its end point in all three point slots, so the end
 * point always sits at the same offsets — which is what lets the canvas read
 * either kind on the same path (the only branch left is `lineTo` vs
 * `bezierCurveTo`).
 */
const CURRENT_SUBPATH: SubpathCursor = {
  startX: 0,
  startY: 0,
  closed: false,
  segBase: 0,
  segments: 0,
  words: new Uint32Array(0),
  floats: new Float32Array(0),
  eachSegment(visit: (segment: SegmentCursor) => void): number {
    let at = CURRENT_SUBPATH.segBase;
    for (let i = 0; i < CURRENT_SUBPATH.segments; i++) {
      CURRENT_SEGMENT.kind = CURRENT_SUBPATH.words[at];
      CURRENT_SEGMENT.c1x = CURRENT_SUBPATH.floats[at + 1];
      CURRENT_SEGMENT.c1y = CURRENT_SUBPATH.floats[at + 2];
      CURRENT_SEGMENT.c2x = CURRENT_SUBPATH.floats[at + 3];
      CURRENT_SEGMENT.c2y = CURRENT_SUBPATH.floats[at + 4];
      CURRENT_SEGMENT.tox = CURRENT_SUBPATH.floats[at + 5];
      CURRENT_SEGMENT.toy = CURRENT_SUBPATH.floats[at + 6];
      visit(CURRENT_SEGMENT);
      at += SEGMENT_WORDS;
    }
    return CURRENT_SUBPATH.segments;
  },
};

/** One segment cursor, reused for every segment. */
const CURRENT_SEGMENT: SegmentCursor = {
  kind: 0,
  c1x: 0,
  c1y: 0,
  c2x: 0,
  c2y: 0,
  tox: 0,
  toy: 0,
};

/** The module's exported surface (the six entry points plus memory). */
export interface EditorExports {
  memory: WebAssembly.Memory;
  editor_call(feature: number, a: number, b: number): number;
  editor_abi_version(): number;
  editor_in_ptr(): number;
  editor_in_cap(): number;
  editor_out_ptr(): number;
  editor_out_cap(): number;
}

/** A result that either carries a value or the module's error code. */
export type EditorResult<T> =
  | { ok: true; value: T }
  | { ok: false; error: number; message: string };

/** Undo/redo bookkeeping the status line shows. */
export interface HistoryView {
  canUndo: boolean;
  canRedo: boolean;
  undoable: number;
  redoable: number;
  dropped: number;
  undoLabel: string | null;
  redoLabel: string | null;
  lastLabel: string | null;
}

/** One interactive editing session. */
export class EditorSession {
  /** The module's exports (advanced use: the canvas never needs them). */
  readonly exports: EditorExports;
  /** The module's own ABI level, checked against `ABI_VERSION` on creation. */
  readonly abiVersion: number;
  /** Bumped by every call that changed the document, selection or history. */
  revision = 0;

  #error: number = ERR.NONE;
  /**
   * The last `NODE_SYNC` result, kept so repeated repaints of an unchanged
   * document cost nothing. Any other call invalidates it (the module's output
   * table is shared by every feature), and a grown memory does too.
   */
  #nodes: { count: number; outBase: number; buffer: ArrayBufferLike } | null = null;

  /** Mirror of the engine's live preview, so the UI can ask without a call. */
  #previewed: EditorCommand | null = null;

  private constructor(exports: EditorExports) {
    this.exports = exports;
    this.abiVersion = exports.editor_abi_version();
  }

  /** Instantiates a module from its bytes. */
  static async fromBytes(bytes: BufferSource): Promise<EditorSession> {
    const { instance } = await WebAssembly.instantiate(bytes, {});
    const exports = instance.exports as unknown as EditorExports;
    if (
      typeof exports.editor_call !== "function" ||
      !(exports.memory instanceof WebAssembly.Memory)
    ) {
      throw new Error("that WebAssembly module is not the Icon Forge editor");
    }
    return new EditorSession(exports);
  }

  /** Fetches the artifact and instantiates it (the app serves it from `public/`). */
  static async fromUrl(url: string): Promise<EditorSession> {
    const response = await fetch(url);
    if (!response.ok) {
      throw new Error(`editor module unavailable: ${response.status} ${response.statusText}`);
    }
    return EditorSession.fromBytes(await response.arrayBuffer());
  }

  /** True when the artifact matches the TypeScript side of the ABI. */
  get compatible(): boolean {
    return this.abiVersion === ABI_VERSION;
  }

  /**
   * Syncs nodes if needed and returns the record region.
   *
   * Repainting an unchanged document calls no ABI feature at all; the revision
   * counter alone would not be enough to trust that, which is why *every* call
   * invalidates the cache rather than only edits.
   */
  #nodeView(): { mem: Uint32Array; outBase: number; count: number } {
    const { mem } = this.#view();
    const cached = this.#nodes;
    if (cached && cached.buffer === mem.buffer) {
      return { mem, outBase: cached.outBase, count: cached.count };
    }
    const count = this.call(FEATURE.NODE_SYNC);
    const { mem: afterMem, outBase: afterBase } = this.#view();
    this.#nodes = { count, outBase: afterBase, buffer: afterMem.buffer };
    return { mem: afterMem, outBase: afterBase, count };
  }

  /** Drops the cached node view, so the next walk re-syncs from the module. */
  invalidateNodes(): void {
    this.#nodes = null;
  }

  /** The error code of the most recent call (`ERR.NONE` when it succeeded). */
  get lastError(): number {
    return this.#error;
  }

  /** The error code of the most recent call, as text. */
  get lastErrorMessage(): string {
    return ERR_TEXT[this.#error] ?? (this.#error === ERR.NONE ? "" : `editor error ${this.#error}`);
  }

  /** Runs one feature, capturing its error code before anything else. */
  call(feature: number, a = 0, b = 0): number {
    // Nothing may survive a call: the output table this cache points into is
    // reused by every feature.
    this.#nodes = null;
    const value = this.exports.editor_call(feature, a, b);
    // ERROR reports the call that just happened, so read it first: any further
    // call would replace it with its own (successful) result.
    this.#error = this.exports.editor_call(FEATURE.ERROR, 0, 0);
    if (feature !== FEATURE.REVISION) {
      this.revision = this.exports.editor_call(FEATURE.REVISION, 0, 0);
    }
    return value;
  }

  /** Word tables, freshly viewed. Never cache the result across a call. */
  #view(): { mem: Uint32Array; inBase: number; outBase: number } {
    const inBase = this.exports.editor_in_ptr() >>> 2;
    const outBase = this.exports.editor_out_ptr() >>> 2;
    return { mem: new Uint32Array(this.exports.memory.buffer), inBase, outBase };
  }

  /** Writes words into the input table. */
  #put(words: Uint32Array | number[], at = 0): void {
    const { mem, inBase } = this.#view();
    if (mem.length < inBase + at + words.length) {
      throw new Error("editor input table overflow");
    }
    mem.set(words, inBase + at);
  }

  /** Copies `len` words out of the output table (for small results). */
  #out(len: number): Uint32Array {
    const { mem, outBase } = this.#view();
    return mem.slice(outBase, outBase + len);
  }

  #fail<T>(): EditorResult<T> {
    return { ok: false, error: this.#error, message: this.lastErrorMessage };
  }

  #label(bytes: number): string {
    return bytes === 0 ? "" : decodeText(this.#out(Math.ceil(bytes / 4)), bytes);
  }

  // -- document ------------------------------------------------------------

  /** Loads a document, replacing whatever was open. Returns the node count. */
  loadDocument(width: number, height: number, nodes: NodeSpec[]): EditorResult<number> {
    this.#put(encodeDoc(width, height, nodes));
    const count = this.call(FEATURE.DOC_LOAD);
    this.#previewed = null;
    if (count === 0 && this.#error !== ERR.NONE) return this.#fail();
    return { ok: true, value: count };
  }

  /** Canvas size of the loaded document. */
  docSize(): { width: number; height: number } | null {
    if (this.call(FEATURE.DOC_SIZE) === 0) return null;
    const words = this.#out(2);
    return { width: w2f(words[0]), height: w2f(words[1]) };
  }

  /** Number of nodes in the document. */
  nodeCount(): number {
    return this.call(FEATURE.NODE_COUNT);
  }

  /** Total path segments (the cost unit the status line reports). */
  segmentCount(): number {
    return this.call(FEATURE.SEGMENT_COUNT);
  }

  /** Every node, bottom first; paths come back in document space. */
  nodes(): NodeSpec[] {
    const nodes: NodeSpec[] = [];
    this.forEachNode((node) => {
      const path: Subpath[] = [];
      node.eachSubpath((sub) => {
        const segs: Segment[] = [];
        sub.eachSegment((seg) => {
          segs.push(
            seg.kind === KIND.CUBIC
              ? { kind: KIND.CUBIC, c1: { x: seg.c1x, y: seg.c1y }, c2: { x: seg.c2x, y: seg.c2y }, to: { x: seg.tox, y: seg.toy } }
              : { kind: KIND.LINE, to: { x: seg.tox, y: seg.toy } },
          );
        });
        path.push({ start: { x: sub.startX, y: sub.startY }, closed: sub.closed, segs });
      });
      nodes.push({
        id: node.id,
        m: Array.from(node.m),
        fill: unpackRgba(node.fill),
        visible: node.visible,
        group: node.group,
        path,
      });
    });
    return nodes;
  }

  /**
   * Walks every node without allocating per-node objects — the per-frame path.
   *
   * The visitor's cursors are **reused** between nodes, subpaths and segments
   * (they are only valid inside their callback), the node transform is a live
   * `Float32Array` view of module memory that stays valid for the whole walk,
   * and numeric reads go straight through a `Float32Array` over the same buffer.
   * Decoding into objects instead costs ~4 µs per segment, which is most of a
   * frame's budget on a 5000-path sheet.
   *
   * @returns the number of nodes visited.
   */
  forEachNode(visit: (node: NodeCursor) => void): number {
    const { mem, outBase, count } = this.#nodeView();
    if (count === 0) return 0;
    const floats = new Float32Array(mem.buffer);
    let at = outBase;
    let visited = 0;
    for (let i = 0; i < count; i++) {
      const id = mem[at];
      // A zero id is the terminator the module writes after the last record:
      // reaching it means we are reading words an earlier call left behind.
      if (id === 0) break;
      const node = NODE_CURSOR;
      node.id = id;
      node.m = floats.subarray(at + 1, at + 7);
      node.fill = mem[at + 7];
      node.visible = mem[at + 8] !== 0;
      node.group = mem[at + NODE_GROUP_SLOT];
      node.pathAt = at + NODE_RECORD_HEADER;
      node.pathWords = mem[at + NODE_PATH_SLOT];
      node.words = mem;
      node.floats = floats;
      visit(node);
      at = node.pathAt + node.pathWords;
      visited += 1;
    }
    return visited;
  }

  /** The placed path of one node (its transform already folded in). */
  pathOf(id: number): Subpath[] | null {
    const words = this.call(FEATURE.PATH_FLUSH, id);
    if (words === 0) return null;
    const { mem, outBase } = this.#view();
    return decodePath(mem, outBase).path;
  }

  /**
   * Parses SVG text into node specs, without needing a document (4C).
   *
   * The module owns the parser: there is no TypeScript one (see
   * `ARCHITECTURE.md` §3.9 — 4A's deviation is retired here), so an icon's
   * geometry is read by exactly the code that draws it. `firstId` numbers the
   * nodes from there, `placement` is folded into each shape's own transform
   * chain, and `fill` is used for shapes the file gives no colour.
   */
  svgNodes(
    text: string,
    firstId: number,
    placement: readonly number[],
    fill: Rgba,
  ): EditorResult<NodeSpec[]> {
    this.#put(encodeSvgSpec(text, firstId, placement, fill));
    const count = this.call(FEATURE.SVG_NODES);
    if (count === 0 && this.#error !== ERR.NONE) return this.#fail();
    return { ok: true, value: this.#records(count) };
  }

  /**
   * Imports SVG text into the open document, one node per `<path>` (4C).
   *
   * One history step, and the imported nodes become the selection — an import
   * that left nothing selected would look like it failed. The engine refuses a
   * file it cannot read (`ERR.MALFORMED_SVG`) and one that draws nothing.
   */
  importSvg(text: string, placement: readonly number[], fill: Rgba): EditorResult<number> {
    this.#put(encodeSvgSpec(text, 0, placement, fill));
    const added = this.call(FEATURE.IMPORT_SVG);
    this.#previewed = null;
    if (added === 0) return this.#fail();
    return { ok: true, value: added };
  }

  /** Decodes `count` node records straight out of the output region. */
  #records(count: number): NodeSpec[] {
    const { mem, outBase } = this.#view();
    return decodeNodeRecords(mem, outBase, count);
  }

  /** The bounding box of one node, in document space. */
  boundsOf(id: number): Box | null {
    if (this.call(FEATURE.NODE_BOUNDS, id) === 0) return null;
    return boxFrom(this.#out(4));
  }

  /** Closes the document and forgets the history. */
  close(): void {
    this.call(FEATURE.CLOSE);
    this.#previewed = null;
  }

  // -- selection -----------------------------------------------------------

  /** The selected node ids, in paint order. */
  selection(): number[] {
    const count = this.call(FEATURE.SELECTION_IDS);
    if (count === 0) return [];
    const { mem, outBase } = this.#view();
    return Array.from(mem.subarray(outBase, outBase + count));
  }

  /** The selection's bounding box. */
  selectionBounds(): Box | null {
    if (this.call(FEATURE.SELECTION_BOUNDS) === 0) return null;
    return boxFrom(this.#out(4));
  }

  selectOnly(id: number): number {
    return this.call(FEATURE.SELECT_ONLY, id);
  }

  selectAdd(id: number): number {
    return this.call(FEATURE.SELECT_ADD, id);
  }

  selectToggle(id: number): number {
    return this.call(FEATURE.SELECT_TOGGLE, id);
  }

  selectAll(): number {
    return this.call(FEATURE.SELECT_ALL);
  }

  clearSelection(): void {
    this.call(FEATURE.SELECT_CLEAR);
  }

  /** The topmost node at a document point (`0` when nothing is there). */
  pick(x: number, y: number): number {
    return this.call(FEATURE.PICK, f2w(x), f2w(y));
  }

  /** The visible nodes whose box intersects a rectangle (corners in any order). */
  marquee(box: Box): number[] {
    this.#put(
      Uint32Array.from([f2w(box.x0), f2w(box.y0), f2w(box.x1), f2w(box.y1)]),
    );
    const count = this.call(FEATURE.MARQUEE);
    if (count === 0) return [];
    const { mem, outBase } = this.#view();
    return Array.from(mem.subarray(outBase, outBase + count));
  }

  /** Hit-test tolerance in document units. */
  get tolerance(): number {
    return w2f(this.call(FEATURE.GET_TOLERANCE));
  }

  set tolerance(value: number) {
    this.call(FEATURE.SET_TOLERANCE, f2w(value));
  }

  // -- edits and history ---------------------------------------------------

  /** Applies a command; the value is the number of node operations it moved. */
  apply(command: EditorCommand): EditorResult<number> {
    this.#put(encodeCommand(command));
    const ops = this.call(FEATURE.APPLY_SPEC);
    // Applying anything drops a live preview (the engine does that too).
    this.#previewed = null;
    if (ops === 0) return this.#fail();
    return { ok: true, value: ops };
  }

  // -- snapping and previews (4B) -------------------------------------------

  /**
   * Asks where a proposed move of the selection would land.
   *
   * The delta asked about is the *proposed* one and the engine answers from the
   * stored geometry, so the caller can preview or commit exactly the delta it
   * gets back — the two can never drift apart.
   */
  snapMove(dx: number, dy: number, query: SnapQuery): EditorResult<SnapResult> {
    this.#put(encodeSnapRequest(dx, dy, query.tolerance, query.flags, query.gridStep));
    this.call(FEATURE.SNAP);
    if (this.#error !== ERR.NONE) return this.#fail();
    return { ok: true, value: decodeSnapResult(this.#out(SNAP_ANSWER_WORDS)) };
  }

  /**
   * Installs a live preview of a command.
   *
   * A preview is *not* an edit: it records no history, moves what `NODE_SYNC`,
   * `NODE_BOUNDS` and `SELECTION_BOUNDS` report, and is dropped by the next
   * `apply`, `undo` or `redo`. Committing the same command afterwards lands on
   * exactly the geometry the preview showed.
   */
  preview(command: EditorCommand): EditorResult<number> {
    this.#put(encodeCommand(command));
    const ops = this.call(FEATURE.PREVIEW, PREVIEW.SET);
    if (ops === 0) return this.#fail();
    this.#previewed = command;
    return { ok: true, value: ops };
  }

  /** Drops the live preview (a cancelled gesture). */
  clearPreview(): void {
    this.call(FEATURE.PREVIEW, PREVIEW.CLEAR);
    this.#previewed = null;
  }

  /**
   * The command currently being previewed, or `null`.
   *
   * The module does not expose this, and it does not need to: the engine drops
   * the preview the moment the document changes, and every path here that
   * changes it drops the mirror too, so the two cannot disagree.
   */
  get previewing(): EditorCommand | null {
    return this.#previewed;
  }

  /** Undoes one step; the value is the undone command's label. */
  undo(): EditorResult<string> {
    const bytes = this.call(FEATURE.UNDO);
    this.#previewed = null;
    if (bytes === 0) return this.#fail();
    return { ok: true, value: this.#label(bytes) };
  }

  /** Redoes one step; the value is the redone command's label. */
  redo(): EditorResult<string> {
    const bytes = this.call(FEATURE.REDO);
    this.#previewed = null;
    if (bytes === 0) return this.#fail();
    return { ok: true, value: this.#label(bytes) };
  }

  /** Everything the status line and the undo buttons need. */
  history(): HistoryView {
    const read = (feature: number): string | null => {
      const bytes = this.call(feature);
      return bytes === 0 ? null : this.#label(bytes);
    };
    return {
      canUndo: this.call(FEATURE.CAN_UNDO) === 1,
      canRedo: this.call(FEATURE.CAN_REDO) === 1,
      undoable: this.call(FEATURE.HISTORY_LEN),
      redoable: this.call(FEATURE.HISTORY_REDO_DEPTH),
      dropped: this.call(FEATURE.HISTORY_DROPPED),
      undoLabel: read(FEATURE.UNDO_LABEL),
      redoLabel: read(FEATURE.REDO_LABEL),
      lastLabel: read(FEATURE.LAST_LABEL),
    };
  }
}

function boxFrom(words: Uint32Array): Box {
  return { x0: w2f(words[0]), y0: w2f(words[1]), x1: w2f(words[2]), y1: w2f(words[3]) };
}
