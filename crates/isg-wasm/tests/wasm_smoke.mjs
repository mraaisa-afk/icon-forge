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
  ERROR: 32,
  SEGMENT_COUNT: 34,
  CLOSE: 36,
};

const OP = { TRANSLATE: 1, SCALE: 2, SET_FILL: 5, VISIBLE: 6, DELETE: 9 };

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
check("abi version", e.editor_abi_version() === 1);

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
const encDoc = (width, height, nodes) => {
  const w = [f2w(width), f2w(height), nodes.length, 0];
  for (const n of nodes) {
    const p = encPath(n.path);
    w.push(
      n.id,
      ...n.m.map(f2w),
      ((n.fill[0] << 24) | (n.fill[1] << 16) | (n.fill[2] << 8) | n.fill[3]) >>> 0,
      n.visible ? 1 : 0,
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

// A longer randomised walk: undo and redo must be exact every single time.
let seed = 0x12345678;
const rnd = () => (seed = (seed * 1103515245 + 12345) & 0x7fffffff) / 0x7fffffff;
const snapshot = () => {
  const records = call(F.NODE_SYNC);
  const { mem, outBase } = views();
  let at = outBase;
  const nodes = [];
  for (let i = 0; i < records; i++) {
    const words = mem[at + 9];
    nodes.push(Array.from(mem.subarray(at, at + 10 + words)).join(":"));
    at += 10 + words;
  }
  const n = call(F.SELECTION_IDS);
  const { mem: mem2, outBase: outBase2 } = views();
  return JSON.stringify([nodes, Array.from(mem2.subarray(outBase2, outBase2 + n))]);
};
let walk = 0;
let exact = true;
for (let i = 0; i < 400 && exact; i++) {
  call(F.SELECT_ALL);
  const spec = [
    [OP.TRANSLATE, f2w(rnd() * 8 - 4), f2w(rnd() * 8 - 4)],
    [OP.SCALE, f2w(0.5 + rnd()), f2w(50), f2w(50)],
    [OP.SET_FILL, (Math.floor(rnd() * 0xffffff) << 8) | 255],
    [OP.VISIBLE, rnd() < 0.5 ? 1 : 0],
    [OP.DELETE],
  ][Math.floor(rnd() * 5)];
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
      `${walk} edits walked with exact undo and redo`,
  );
}
process.exit(failures.length ? 1 : 0);
