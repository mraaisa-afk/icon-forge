// Runs the *built* wasm artifact under Node.
//
// The Rust tests exercise the ABI through the same dispatcher, but they cannot
// prove the thing that ships: that the artifact exports the expected symbols,
// that its memory is exported, that its tables really are inside that memory,
// and that the byte-level protocol works from JavaScript. That is what this
// script checks, on the exact bytes `ci.yml` builds.
//
// Usage: node crates/isg-wasm/tests/wasm_smoke.mjs [path-to-wasm]
import { readFileSync } from "node:fs";
import process from "node:process";

const wasmPath =
  process.argv[2] ?? "target/wasm32-unknown-unknown/release/isg_wasm.wasm";

const F = {
  VERSION: 0,
  DOC_LOAD: 1,
  NODE_COUNT: 2,
  REVISION: 3,
  NODE_SYNC: 4,
  NODE_BOUNDS: 5,
  PATH_FLUSH: 6,
  SELECTION_COUNT: 7,
  SELECTION_IDS: 9,
  SELECTION_BOUNDS: 10,
  SELECT_ALL: 11,
  SELECT_ONLY: 13,
  PICK: 16,
  MARQUEE: 17,
  APPLY_SPEC: 20,
  UNDO: 21,
  REDO: 22,
  CAN_UNDO: 23,
  HISTORY_LEN: 25,
  NODE_AT: 35,
  ERROR: 32,
  SEGMENT_COUNT: 34,
  CLOSE: 36,
  SNAP: 37,
  PREVIEW: 38,
  SVG_NODES: 39,
  IMPORT_SVG: 40,
};

const OP = {
  TRANSLATE: 1,
  SCALE: 2,
  SET_FILL: 5,
  VISIBLE: 6,
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
};

// SNAP flags / PREVIEW arguments.
const SNAP = { CANVAS: 1, NODES: 2, GRID: 4, FLAGS: 7 };
const PREVIEW = { SET: 0, CLEAR: 1 };

const failures = [];
const check = (name, ok, detail = "") => {
  if (!ok) failures.push(name);
  console.log(`${ok ? "ok  " : "FAIL"} ${name}${detail ? ` — ${detail}` : ""}`);
};

const bytes = readFileSync(wasmPath);
const { instance } = await WebAssembly.instantiate(bytes, {});
const e = instance.exports;
const f2w = (v) => {
  const b = new ArrayBuffer(4);
  new DataView(b).setFloat32(0, v, true);
  return new Uint32Array(b)[0];
};
const w2f = (w) => {
  const b = new ArrayBuffer(4);
  new Uint32Array(b)[0] = w;
  return new DataView(b).getFloat32(0, true);
};

// --- the module's own contract with the adapter ------------------------------
const expected = [
  "memory",
  "editor_call",
  "editor_abi_version",
  "editor_in_ptr",
  "editor_in_cap",
  "editor_out_ptr",
  "editor_out_cap",
];
check(
  "exports the adapter's symbols",
  expected.every((name) => name in e),
  Object.keys(e).join(","),
);
check("exports memory", e.memory instanceof WebAssembly.Memory);
check("abi version", e.editor_abi_version() === 2);

// Views must be rebuilt after every call: growing linear memory detaches the
// previous buffer, and a stale view would silently read zeros.
const views = () => {
  // Touch the accessors first: the first call allocates both tables, which may
  // grow linear memory. A view (and its length) must be taken afterwards — and
  // re-taken after every later call that could grow it again.
  const inBase = e.editor_in_ptr() >> 2;
  const outBase = e.editor_out_ptr() >> 2;
  const mem = new Uint32Array(e.memory.buffer);
  return { mem, inBase, outBase, words: mem.length };
};
const first = views();
check(
  "input table lives in memory",
  first.inBase + e.editor_in_cap() <= first.words,
  `in ${first.inBase}+${e.editor_in_cap()} of ${first.words}`,
);
check(
  "output table lives in memory",
  first.outBase + e.editor_out_cap() <= first.words,
  `out ${first.outBase}+${e.editor_out_cap()} of ${first.words}`,
);

const call = (feature, a = 0, b = 0) => e.editor_call(feature, a, b);
const put = (words) => {
  const { mem, inBase } = views();
  mem.set(words, inBase);
};
const out = (len) => {
  const { mem, outBase } = views();
  return Array.from(mem.subarray(outBase, outBase + len));
};
const error = () => call(F.ERROR);

// --- protocol -----------------------------------------------------------------
check("no document is an error", call(F.NODE_COUNT) === 0 && error() === 2);

const encPath = (subpaths) => {
  const w = [subpaths.length];
  for (const sp of subpaths) {
    w.push(f2w(sp.start[0]), f2w(sp.start[1]), sp.closed ? 1 : 0, sp.segs.length);
    for (const s of sp.segs) {
      w.push(
        s.kind,
        f2w(s.c1[0]),
        f2w(s.c1[1]),
        f2w(s.c2[0]),
        f2w(s.c2[1]),
        f2w(s.to[0]),
        f2w(s.to[1]),
      );
    }
  }
  return w;
};
const square = (x, y, s) => ({
  start: [x, y],
  closed: true,
  segs: [
    { kind: 0, c1: [x + s, y], c2: [x + s, y], to: [x + s, y] },
    { kind: 0, c1: [x + s, y + s], c2: [x + s, y + s], to: [x + s, y + s] },
    { kind: 0, c1: [x, y + s], c2: [x, y + s], to: [x, y + s] },
  ],
});
// Node record: id, m[6], fill, visible, group, path word count (including the
// count word itself), then the path blob.
const encDoc = (width, height, nodes) => {
  const w = [f2w(width), f2w(height), nodes.length, 0];
  for (const n of nodes) {
    const p = encPath(n.path);
    w.push(
      n.id,
      ...n.m.map(f2w),
      ((n.fill[0] << 24) | (n.fill[1] << 16) | (n.fill[2] << 8) | n.fill[3]) >>> 0,
      n.visible ? 1 : 0,
      n.group ?? 0,
      p.length,
      ...p,
    );
  }
  w[3] = w.length;
  return w;
};

put(
  encDoc(200, 120, [
    { id: 1, m: [1, 0, 0, 1, 0, 0], fill: [255, 0, 0, 255], visible: true, path: [square(10, 10, 20)] },
    { id: 2, m: [1, 0, 0, 1, 50, 20], fill: [0, 255, 0, 255], visible: true, path: [square(0, 0, 30)] },
  ]),
);
check("doc_load", call(F.DOC_LOAD) === 2, `nodes=${call(F.NODE_COUNT)}`);
check("segment_count", call(F.SEGMENT_COUNT) === 6);
check("pick hits the top node", call(F.PICK, f2w(55), f2w(25)) === 2);
put([f2w(0), f2w(0), f2w(45), f2w(45)]);
check("marquee", call(F.MARQUEE) === 1 && out(2)[0] === 1 && out(2)[1] === 0);

// A drag: apply, undo, redo — state must come back bit for bit.
check("select", call(F.SELECT_ONLY, 2) === 1);
put([OP.TRANSLATE, f2w(5), f2w(-3)]);
check("apply translate", call(F.APPLY_SPEC) === 1);
call(F.NODE_BOUNDS, 2);
const moved = out(4).map(w2f);
check("moved bounds", moved.join(",") === "55,17,85,47", moved.join(","));
check("undo", call(F.UNDO) === 4, `label bytes; error=${error()}`);
call(F.NODE_BOUNDS, 2);
const restored = out(4).map(w2f);
check("undo restored bits", restored.join(",") === "50,20,80,50", restored.join(","));
check("redo", call(F.REDO) === 4);
call(F.NODE_BOUNDS, 2);
check("redo restored bits", out(4).map(w2f).join(",") === "55,17,85,47");
check("undo again", call(F.UNDO) === 4);
check("nothing left to undo", call(F.UNDO) === 0 && error() === 5);
check("history len", call(F.HISTORY_LEN) === 0 && call(F.CAN_UNDO) === 0);

// --- 4B: preview, snap, groups, arrange/align, non-uniform scale ------------
// A preview moves what the view reports — the node record and the selection box
// — without touching the document or the history.
check("select for preview", call(F.SELECT_ONLY, 2) === 1);
call(F.NODE_BOUNDS, 2);
const stored_bounds = out(4).map(w2f);
put([OP.TRANSLATE, f2w(11), f2w(7)]);
check(
  "preview_set previews the selection",
  call(F.PREVIEW, PREVIEW.SET) === 1 && error() === 0,
);
call(F.NODE_BOUNDS, 2);
const previewed = out(4).map(w2f);
check(
  "node bounds follow the preview",
  previewed.join(",") === "61,27,91,57",
  previewed.join(","),
);
check(
  "a preview is not history",
  call(F.CAN_UNDO) === 0 && call(F.HISTORY_LEN) === 0,
);
call(F.SELECTION_BOUNDS);
check(
  "the selection box follows the preview",
  out(4).map(w2f).join(",") === previewed.join(","),
  out(4).map(w2f).join(","),
);
check("preview_clear", call(F.PREVIEW, PREVIEW.CLEAR) === 0);
call(F.NODE_BOUNDS, 2);
check("clearing restores the stored bounds", out(4).map(w2f).join(",") === stored_bounds.join(","));

// Committing the same spec lands on exactly the geometry the preview showed.
put([OP.TRANSLATE, f2w(11), f2w(7)]);
check("apply the previewed spec", call(F.APPLY_SPEC) === 1);
call(F.NODE_BOUNDS, 2);
check(
  "the commit matches the preview",
  out(4).map(w2f).join(",") === previewed.join(","),
);
check("commit is one history step", call(F.HISTORY_LEN) === 1);
call(F.UNDO);

// Snapping: node 2 sits at x 50..80, y 20..50, so a +7,+0 nudge lands its
// centre on the 8-unit grid in x (no correction needed) and its edge two units
// off it in y. The answer carries both facts: the corrected delta plus one
// guide per snapped position, and the list is terminated.
put([f2w(7), f2w(0), f2w(6), SNAP.GRID, f2w(8)]);
const guides = call(F.SNAP);
const snapWords = out(3 + guides * 5 + 1);
check(
  "snap corrects the delta onto the grid",
  guides === 2 &&
    Math.abs(w2f(snapWords[0]) - 7) < 1e-3 &&
    w2f(snapWords[1]) === -2,
  `guides=${guides} dx=${w2f(snapWords[0])} dy=${w2f(snapWords[1])}`,
);
const guideRecords = [];
for (let i = 0; i < guides; i++) {
  const at = 3 + i * 5;
  guideRecords.push({
    axis: snapWords[at],
    kind: snapWords[at + 1],
    position: w2f(snapWords[at + 2]),
  });
}
check(
  "snap guides are typed, on the grid, and terminated",
  guideRecords.every(
    (g) => g.kind === 4 && g.axis <= 1 && Math.abs(g.position % 8) < 1e-3,
  ) &&
    guideRecords.map((g) => g.axis).join(",") === "0,1" &&
    snapWords[3 + guides * 5] === 0,
  guideRecords.map((g) => `${g.axis}:${g.kind}:${g.position}`).join(" "),
);
put([f2w(7), f2w(0), f2w(6), SNAP.FLAGS + 1, f2w(8)]);
check("snap refuses an undefined flag bit", call(F.SNAP) === 0 && error() === 3);

// Groups: a group id travels in the node record, and ungrouping clears it.
call(F.SELECT_ALL);
put([OP.GROUP]);
check("group", call(F.APPLY_SPEC) === 2);
let records = call(F.NODE_SYNC);
{
  const { mem, outBase } = views();
  const g = [];
  let at = outBase;
  for (let i = 0; i < records; i++) {
    g.push(mem[at + 9]);
    at += 11 + mem[at + 10];
  }
  check(
    "both nodes carry one shared group id",
    g.length === 2 && g[0] !== 0 && g[0] === g[1],
    g.join(","),
  );
}
put([OP.UNGROUP]);
check("ungroup", call(F.APPLY_SPEC) === 2);
records = call(F.NODE_SYNC);
{
  const { mem, outBase } = views();
  const g = [];
  let at = outBase;
  for (let i = 0; i < records; i++) {
    g.push(mem[at + 9]);
    at += 11 + mem[at + 10];
  }
  check("group ids are cleared", g.every((v) => v === 0), g.join(","));
}

// Align (to the selection's left edge), arrange (one node to the front) and a
// non-uniform scale, all through their specs.
// The two squares are 40 apart in x, so aligning to the selection's left edge
// moves exactly one of them (and leaves the other alone).
call(F.SELECT_ALL);
put([OP.ALIGN, 0, 0]);
check("align to the selection frame", call(F.APPLY_SPEC) === 1);
call(F.SELECT_ONLY, 2);
call(F.NODE_BOUNDS, 2);
check(
  "both left edges line up",
  out(4).map(w2f).join(",") === "10,20,40,50",
  out(4).map(w2f).join(","),
);
call(F.UNDO);
// Arranging the back-most node to the front really does reorder the document.
const back = call(F.NODE_AT, 0);
call(F.SELECT_ONLY, back);
put([OP.ARRANGE, 0]);
check("arrange the back node to the front", call(F.APPLY_SPEC) === 2);
check("the node order changed", call(F.NODE_AT, 1) === back);
call(F.UNDO);
call(F.SELECT_ALL);
put([OP.SCALE_XY, f2w(2), f2w(0.5), f2w(0), f2w(0)]);
check("scale_x_y", call(F.APPLY_SPEC) === 2);
call(F.SELECT_ONLY, 1);
call(F.NODE_BOUNDS, 1);
check(
  "a non-uniform scale stretches x and squashes y about the pivot",
  out(4).map(w2f).join(",") === "20,5,60,15",
  out(4).map(w2f).join(","),
);
call(F.UNDO);
while (call(F.CAN_UNDO)) call(F.UNDO);
check("back to a clean history", call(F.HISTORY_LEN) === 0);

// --- 4C: point editing, booleans and SVG import ------------------------------
// Node 1 is the 10..30 square. Its first vertex is the only one that is not on
// the origin corner, so moving it is visible in the record.
call(F.SELECT_ONLY, 1);
put([OP.MOVE_POINT, 1, 0, 1, f2w(35), f2w(12)]);
check("move_point", call(F.APPLY_SPEC) === 1, `error=${error()}`);
// The flush carries the placed path: subpath count, then start x/y, closed,
// segment count, then 7 words per segment (kind, c1, c2, to). Vertex 1 is the
// end point of segment 0, i.e. words 10 and 11.
const segmentEnd = (id) => {
  call(F.PATH_FLUSH, id);
  const w = out(12).map(w2f);
  return [w[10], w[11]];
};
check(
  "the moved vertex is in the flushed path",
  segmentEnd(1).every((v, i) => Math.abs(v - [35, 12][i]) < 1e-3),
  segmentEnd(1).join(","),
);
check("undo the point edit", call(F.UNDO) === 5, "label bytes for 'point'");
check(
  "undo restored the vertex",
  segmentEnd(1).every((v, i) => Math.abs(v - [30, 10][i]) < 1e-3),
  segmentEnd(1).join(","),
);

// Insert a vertex on the first segment, convert it to a cubic, drag one of its
// handles, then delete the inserted vertex again — five edits, five steps.
put([OP.INSERT_POINT, 1, 0, 0, f2w(0.5)]);
check("insert_point", call(F.APPLY_SPEC) === 1);
check("segment count grew", call(F.SEGMENT_COUNT) === 7);
put([OP.SET_SEGMENT, 1, 0, 0, 1]);
check("set_segment to cubic", call(F.APPLY_SPEC) === 1);
put([OP.MOVE_HANDLE, 1, 0, 0, 0, f2w(22), f2w(-6)]);
check("move_handle", call(F.APPLY_SPEC) === 1);
put([OP.DELETE_POINT, 1, 0, 1]);
check("delete_point", call(F.APPLY_SPEC) === 1);
check("segment count is back", call(F.SEGMENT_COUNT) === 6);
// The undone move was dropped when the next edit pushed its own step, so the
// cursor sits at the four edits that followed it.
check("four more history steps", call(F.HISTORY_LEN) === 4);
while (call(F.CAN_UNDO)) call(F.UNDO);
check("the point edits undo back to a clean history", call(F.HISTORY_LEN) === 0);
check(
  "and back to the original square",
  segmentEnd(1).every((v, i) => Math.abs(v - [30, 10][i]) < 1e-3),
  segmentEnd(1).join(","),
);

// A boolean over the wire: the two squares overlap by 20 in each axis, so the
// union is one 60x60 node and the undo brings both back.
call(F.SELECT_ALL);
put([OP.BOOLEAN, 0]);
// Two ops: the path edit and the removal of the second node. Neither node
// carries a placement, so the result needs no transform op.
check("boolean union", call(F.APPLY_SPEC) === 2, `error=${error()}`);
check("one node left", call(F.NODE_COUNT) === 1);
call(F.NODE_BOUNDS, 1);
check(
  "the union spans both squares",
  out(4)
    .map(w2f)
    .every((v, i) => Math.abs(v - [10, 10, 80, 50][i]) < 1e-3),
  out(4).map(w2f).join(","),
);
check("undo the boolean", call(F.UNDO) === 7);
check("both nodes are back", call(F.NODE_COUNT) === 2);

// SVG text, both ways: parsed with no document at all, then imported into one.
const svgText =
  '<svg viewBox="0 0 32 32">' +
  '<path d="M4,4 L28,4 L28,28 L4,28 Z" fill="#123456"/>' +
  '<g transform="translate(2,2)"><path d="M8,8 C12,4 20,4 24,8"/></g>' +
  "</svg>";
const putSvg = (firstId, m, fill) => {
  const bytes =
    typeof TextEncoder === "undefined"
      ? Uint8Array.from(svgText, (c) => c.charCodeAt(0))
      : new TextEncoder().encode(svgText);
  const w = [firstId, ...m.map(f2w), fill >>> 0, bytes.length];
  for (let i = 0; i < bytes.length; i += 4) {
    let word = 0;
    for (let j = 0; j < 4 && i + j < bytes.length; j++) word |= bytes[i + j] << (8 * j);
    w.push(word >>> 0);
  }
  put(w);
};
check(
  "close before the import test",
  call(F.CLOSE) === 0 && call(F.NODE_COUNT) === 0 && error() === 2,
);
putSvg(7, [1, 0, 0, 1, 0, 0], 0x000000ff);
const parsedRecords = call(F.SVG_NODES);
check(
  "svg_nodes needs no document",
  parsedRecords === 2 && error() === 0,
  `records=${parsedRecords} error=${error()}`,
);
{
  const { mem, outBase } = views();
  const ids = [mem[outBase], mem[outBase + 11 + mem[outBase + 10]]];
  const fill = mem[outBase + 7];
  check(
    "svg_nodes reports the file's geometry and fills",
    ids.join(",") === "7,8" && fill === 0x123456ff,
    `ids=${ids.join(",")} fill=${fill.toString(16)}`,
  );
}
put(
  encDoc(100, 100, [
    { id: 1, m: [1, 0, 0, 1, 0, 0], fill: [9, 9, 9, 255], visible: true, path: [square(0, 0, 5)] },
  ]),
);
check("load a document to import into", call(F.DOC_LOAD) === 1);
putSvg(1, [1, 0, 0, 1, 10, 20], 0x090909ff);
check(
  "import_svg adds one node per path",
  call(F.IMPORT_SVG) === 2 && call(F.NODE_COUNT) === 3,
  `error=${error()}`,
);
check("the import is one history step", call(F.HISTORY_LEN) === 1);
check("undo the import", call(F.UNDO) === 10);
check("nothing of it is left", call(F.NODE_COUNT) === 1);
putSvg(1, [1, 0, 0, 1, 0, 0], 0x090909ff);
// REDO answers with the byte length of the label it replayed, like UNDO.
check("import it again", call(F.REDO) === 10 && call(F.NODE_COUNT) === 3);
// Malformed text is refused with its own code, not silently skipped.
{
  const bad = '<svg><path d="M0,0 B1,1"/></svg>';
  const bytes = Uint8Array.from(bad, (c) => c.charCodeAt(0));
  const w = [1, f2w(1), f2w(0), f2w(0), f2w(1), f2w(0), f2w(0), 0x090909ff, bytes.length];
  for (let i = 0; i < bytes.length; i += 4) {
    let word = 0;
    for (let j = 0; j < 4 && i + j < bytes.length; j++) word |= bytes[i + j] << (8 * j);
    w.push(word >>> 0);
  }
  put(w);
  check(
    "malformed svg text is refused",
    call(F.SVG_NODES) === 0 && error() === 11,
    `error=${error()}`,
  );
}
while (call(F.CAN_UNDO)) call(F.UNDO);
call(F.CLOSE);

// Reload the fixture the walk expects, now that the import section closed it.
put(
  encDoc(200, 120, [
    { id: 1, m: [1, 0, 0, 1, 0, 0], fill: [255, 0, 0, 255], visible: true, path: [square(10, 10, 20)] },
    { id: 2, m: [1, 0, 0, 1, 50, 20], fill: [0, 255, 0, 255], visible: true, path: [square(0, 0, 30)] },
  ]),
);
check("reload the walk fixture", call(F.DOC_LOAD) === 2);

// A longer randomised walk: undo and redo must be exact every single time.
let seed = 0x12345678;
const rnd = () => (seed = (seed * 1103515245 + 12345) & 0x7fffffff) / 0x7fffffff;
const snapshot = () => {
  const records = call(F.NODE_SYNC);
  const { mem, outBase } = views();
  let at = outBase;
  const nodes = [];
  for (let i = 0; i < records; i++) {
    const words = mem[at + 10];
    nodes.push(Array.from(mem.subarray(at, at + 11 + words)).join(":"));
    at += 11 + words;
  }
  const n = call(F.SELECTION_IDS);
  const { mem: mem2, outBase: outBase2 } = views();
  return JSON.stringify([nodes, Array.from(mem2.subarray(outBase2, outBase2 + n))]);
};
let walk = 0;
let exact = true;
for (let i = 0; i < 400 && exact; i++) {
  call(F.SELECT_ALL);
  const top = call(F.NODE_AT, 0);
  const spec = [
    [OP.TRANSLATE, f2w(rnd() * 8 - 4), f2w(rnd() * 8 - 4)],
    [OP.SCALE, f2w(0.5 + rnd()), f2w(50), f2w(50)],
    [OP.SET_FILL, (Math.floor(rnd() * 0xffffff) << 8) | 255],
    [OP.VISIBLE, rnd() < 0.5 ? 1 : 0],
    [OP.DELETE],
    [OP.SCALE_XY, f2w(0.5 + rnd()), f2w(0.5 + rnd()), f2w(50), f2w(50)],
    [OP.ALIGN, 0, Math.floor(rnd() * 6)],
    [OP.GROUP],
    [OP.UNGROUP],
    // 4C: the newest geometry joins the walk, addressed at the top node's first
    // subpath. A spec that no longer fits the geometry is refused and skipped —
    // that is the same contract the adapter relies on.
    [OP.MOVE_POINT, top, 0, 1, f2w(rnd() * 40), f2w(rnd() * 40)],
    [OP.INSERT_POINT, top, 0, 0, f2w(0.1 + rnd() * 0.8)],
    [OP.SET_SEGMENT, top, 0, 0, rnd() < 0.5 ? 0 : 1],
    [OP.MOVE_HANDLE, top, 0, 0, rnd() < 0.5 ? 0 : 1, f2w(rnd() * 40), f2w(rnd() * 40)],
    [OP.DELETE_POINT, top, 0, 1],
    [OP.BOOLEAN, Math.floor(rnd() * 4)],
  ][Math.floor(rnd() * 15)];
  const before = snapshot();
  put(spec);
  if (call(F.APPLY_SPEC) === 0) continue;
  walk += 1;
  const after = snapshot();
  call(F.UNDO);
  if (snapshot() !== before) {
    exact = false;
    console.log("  undo diverged for", JSON.stringify(spec));
  }
  call(F.REDO);
  if (snapshot() !== after) {
    exact = false;
    console.log("  redo diverged for", JSON.stringify(spec));
  }
  call(F.UNDO);
}
check(`random walk is exact (${walk} edits)`, exact && walk > 250);
check("close", call(F.CLOSE) === 0 && call(F.NODE_COUNT) === 0 && error() === 2);

console.log(
  failures.length
    ? `\n${failures.length} wasm smoke failures: ${failures.join(" | ")}`
    : "\nwasm smoke: all checks green",
);
if (!failures.length) {
  console.log(
    `evidence: wasm artifact — ${bytes.length} bytes, ABI v${e.editor_abi_version()}, ` +
      `${instance.exports.memory.buffer.byteLength >>> 20} MiB memory after the tables allocate, ` +
      `${walk} edits walked with exact undo and redo (including groups, ` +
      `align, non-uniform scale, point editing, booleans and SVG import ` +
      `in both directions)`,
  );
}
// `process.exit()` would drop buffered writes when stdout is a pipe (it is, in
// CI, where this output is teed into the run's evidence): set the code and let
// Node flush and exit on its own.
process.exitCode = failures.length ? 1 : 0;
