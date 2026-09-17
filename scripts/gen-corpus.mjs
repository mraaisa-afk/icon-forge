// JS mirror of tools/gen-corpus (the Rust generator).
//
// Determinism contract: the RNG stream is consumed in exactly the same order
// and all rasterization is integer-only, so this script and the Rust tool
// produce bit-identical pixel sets and truth JSONs. CI regenerates with the
// Rust tool and diffs the JSON sidecars byte-for-byte. Only PNG/JPEG container
// bytes may differ (encoder detail, not part of the contract).
//
// Run from the repo root:  node scripts/gen-corpus.mjs [--out bench/corpus]
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { encodePng, encodeJpegGray } from "./pngio.mjs";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const MASK64 = (1n << 64n) - 1n;

/// LCG with the numerical-recipes constants — mirrors the Rust `Rng`.
class Rng {
  constructor(seed) {
    this.s = seed & MASK64;
  }
  nextU64() {
    this.s = (this.s * 6364136223846348886n + 1442695040888963407n) & MASK64;
    return this.s;
  }
  /// Uniform integer in [lo, hi] (inclusive).
  range(lo, hi) {
    if (hi <= lo) return lo;
    return lo + Number(this.nextU64() % BigInt(hi - lo + 1));
  }
  /// Uniform signed integer in [lo, hi] (inclusive).
  rangeI(lo, hi) {
    if (hi <= lo) return lo;
    return lo + Number(this.nextU64() % BigInt(hi - lo + 1));
  }
  pick(v) {
    return v[Number(this.nextU64() % BigInt(v.length))];
  }
}

/// Integer truncating division (matches Rust `i64 / i64`).
function idiv(a, b) {
  return Math.trunc(a / b);
}

function rasterizeShape(shape, s, t, cell) {
  const S = s;
  const T = t;
  const c = idiv(S - 1, 2);
  let minx = Infinity;
  let miny = Infinity;
  let maxx = -Infinity;
  let maxy = -Infinity;
  for (let y = 0; y < S; y++) {
    for (let x = 0; x < S; x++) {
      const ux = x - c;
      const uy = y - c;
      const dx = Math.abs(ux);
      const dy = Math.abs(uy);
      const d2 = (100 * dx) * (100 * dx) + (100 * dy) * (100 * dy);
      let fill = false;
      switch (shape) {
        case "square":
          fill = true;
          break;
        case "circle": {
          const r = 48 * S;
          fill = d2 <= r * r;
          break;
        }
        case "ring": {
          const r = 48 * S;
          const ir = Math.max(r - 100 * T, 1);
          fill = d2 <= r * r && d2 >= ir * ir;
          break;
        }
        case "frame": {
          const r = 46 * S;
          const ir = Math.max(r - 100 * T, 1);
          fill = d2 <= r * r && d2 >= ir * ir;
          break;
        }
        case "cross":
          fill = dx <= T || dy <= T;
          break;
        case "plus": {
          const m = idiv(38 * S, 100);
          fill = (dx <= T && dy <= m) || (dy <= T && dx <= m);
          break;
        }
        case "diamond":
          fill = 100 * (dx + dy) <= 48 * S;
          break;
        case "triangle": {
          const h = idiv(96 * S, 100);
          const half = idiv(48 * S, 100);
          const span = idiv((uy + idiv(h, 2)) * half, h);
          fill = Math.abs(uy) <= idiv(h, 2) && dx <= Math.max(span, 0);
          break;
        }
        case "lshape": {
          const a = idiv(S, 2);
          fill = uy >= a - T || ux >= a - T;
          break;
        }
        case "bars": {
          const a = idiv(44 * S, 100);
          const band = idiv(30 * S, 100);
          const halfT = idiv(T, 2);
          fill =
            (ux <= -(a - T) && dy <= a) ||
            (dx <= a && (dy <= halfT || Math.abs(dy - band) <= halfT));
          break;
        }
        default:
          fill = false;
      }
      if (fill) {
        cell[y * S + x] = 1;
        if (x < minx) minx = x;
        if (y < miny) miny = y;
        if (x > maxx) maxx = x;
        if (y > maxy) maxy = y;
      }
    }
  }
  if (maxx < minx) return null;
  return { x: minx, y: miny, w: maxx - minx + 1, h: maxy - miny + 1 };
}

class Sheet {
  constructor(w, h, bg) {
    this.w = w;
    this.h = h;
    this.px = new Uint8Array(w * h).fill(bg);
    this.placed = [];
  }
  intersectsPadded(a, b, gap) {
    return (
      a.x < b.x + b.w + gap &&
      b.x < a.x + a.w + gap &&
      a.y < b.y + b.h + gap &&
      b.y < a.y + a.h + gap
    );
  }
  stamp(shape, tx, ty, s, t, color) {
    const cell = new Uint8Array(s * s);
    const bb = rasterizeShape(shape, s, t, cell);
    if (!bb) return null;
    const sx = tx + bb.x;
    const sy = ty + bb.y;
    if (sx < 0 || sy < 0 || sx + bb.w > this.w || sy + bb.h > this.h) return null;
    for (let y = bb.y; y < bb.y + bb.h; y++) {
      for (let x = bb.x; x < bb.x + bb.w; x++) {
        if (cell[y * s + x] === 1) {
          this.px[(sy + (y - bb.y)) * this.w + (sx + (x - bb.x))] = color;
        }
      }
    }
    const out = { x: sx, y: sy, w: bb.w, h: bb.h };
    this.placed.push(out);
    return out;
  }
}

const SHAPES = [
  "square", "circle", "ring", "cross", "plus", "diamond", "triangle", "frame", "lshape", "bars",
];

function strokeFor(rng, shape, s) {
  switch (shape) {
    case "ring":
    case "frame":
      return Math.max(rng.range(Math.floor(s / 8), Math.floor(s / 6)), 2);
    case "cross":
    case "plus":
      return Math.max(rng.range(Math.floor(s / 10), Math.floor(s / 8)), 2);
    case "lshape":
      return Math.max(rng.range(Math.floor(s / 8), Math.floor(s / 6)), 2);
    default:
      return Math.max(rng.range(Math.floor(s / 10), Math.floor(s / 7)), 2);
  }
}

function buildGrid(rng, sheet, truth, cfg) {
  const margin = idiv(sheet.w, Math.max(2 * cfg.n, 1));
  const cell = idiv(sheet.w - 2 * margin, cfg.n);
  for (let row = 0; row < cfg.n; row++) {
    for (let col = 0; col < cfg.n; col++) {
      const s = rng.range(cfg.sizes[0], cfg.sizes[1]);
      const dx = cfg.drift > 0 ? rng.rangeI(-cfg.drift, cfg.drift) : 0;
      const dy = cfg.drift > 0 ? rng.rangeI(-cfg.drift, cfg.drift) : 0;
      let tx = margin + col * cell + idiv(cell - s, 2) + dx;
      let ty = margin + row * cell + idiv(cell - s, 2) + dy;
      const lo = idiv(margin, 2);
      tx = Math.min(Math.max(tx, lo), Math.max(sheet.w - s - idiv(margin, 2), lo));
      ty = Math.min(Math.max(ty, lo), Math.max(sheet.h - s - idiv(margin, 2), lo));
      const shape = rng.pick(SHAPES);
      const t = strokeFor(rng, shape, s);
      const color = rng.range(cfg.colors[0], cfg.colors[1]);
      const bbox = sheet.stamp(shape, tx, ty, s, t, color);
      if (bbox !== null) {
        truth.push({ id: truth.length, shape, bbox });
      } else {
        const tx2 = margin + col * cell + idiv(cell - s, 2);
        const ty2 = margin + row * cell + idiv(cell - s, 2);
        const bbox2 = sheet.stamp(shape, tx2, ty2, s, t, color);
        if (bbox2 !== null) truth.push({ id: truth.length, shape, bbox: bbox2 });
      }
    }
  }
}

function buildScattered(rng, sheet, truth, count, sizes, gap) {
  const cols = 6;
  const rows = Math.floor((count + cols - 1) / cols);
  const marginX = idiv(sheet.w, Math.max(2 * cols, 1));
  const marginY = idiv(sheet.h, Math.max(2 * rows, 1));
  const cellw = idiv(sheet.w - 2 * marginX, cols);
  const cellh = idiv(sheet.h - 2 * marginY, rows);
  const maxS = sizes[1];
  const jitter = Math.max(idiv(Math.min(cellw, cellh) - maxS - gap, 4), 2);
  for (let i = 0; i < count; i++) {
    const col = i % cols;
    const row = Math.floor(i / cols);
    const s = rng.range(sizes[0], sizes[1]);
    const shape = rng.pick(SHAPES);
    const t = strokeFor(rng, shape, s);
    const color = rng.range(0, 40);
    const tx = marginX + col * cellw + idiv(cellw - s, 2) + rng.rangeI(-jitter, jitter);
    const ty = marginY + row * cellh + idiv(cellh - s, 2) + rng.rangeI(-jitter, jitter);
    const cand = { x: tx, y: ty, w: s, h: s };
    if (sheet.placed.some((p) => sheet.intersectsPadded(p, cand, gap))) {
      throw new Error(`scattered layout violated the ${gap}px gap`);
    }
    const bbox = sheet.stamp(shape, tx, ty, s, t, color);
    if (bbox === null) throw new Error(`scattered stamp failed for shape ${shape}`);
    truth.push({ id: truth.length, shape, bbox });
  }
}

function buildEdgeCorner(rng, sheet, truth) {
  const w = sheet.w;
  const h = sheet.h;
  const s = 56;
  const m = 2;
  const spots = [
    [m, m],
    [w - s - m, m],
    [m, h - s - m],
    [w - s - m, h - s - m],
    [idiv(w, 2) - idiv(s, 2), m],
    [idiv(w, 2) - idiv(s, 2), h - s - m],
    [m, idiv(h, 2) - idiv(s, 2)],
    [w - s - m, idiv(h, 2) - idiv(s, 2)],
    [idiv(w, 3), idiv(h, 3)],
    [idiv(2 * w, 3) - s, idiv(h, 3)],
    [idiv(w, 3), idiv(2 * h, 3) - s],
    [idiv(2 * w, 3) - s, idiv(2 * h, 3) - s],
  ];
  spots.forEach(([tx, ty], i) => {
    const shape = SHAPES[(i * 3 + rng.range(0, 2)) % SHAPES.length];
    const t = rng.range(4, 8);
    const color = rng.range(0, 30);
    const bbox = sheet.stamp(shape, tx, ty, s, t, color);
    if (bbox === null) throw new Error(`edge_corner stamp failed at ${tx},${ty}`);
    truth.push({ id: truth.length, shape, bbox });
  });
}

/// C9 seeding: 4×4 near-duplicate grid (small shape set, ±2 px jitter).
function buildDuplicates(rng, sheet, truth) {
  const n = 4;
  const set = ["circle", "square", "diamond", "ring"];
  const margin = idiv(sheet.w, 2 * n);
  const cell = idiv(sheet.w - 2 * margin, n);
  for (let row = 0; row < n; row++) {
    for (let col = 0; col < n; col++) {
      const s = 48 + rng.rangeI(-2, 2);
      const shape = set[(row * n + col) % set.length];
      const t = strokeFor(rng, shape, s);
      const color = rng.nextU64() % 4n === 0n ? 20 : 0;
      const tx = margin + col * cell + idiv(cell - s, 2);
      const ty = margin + row * cell + idiv(cell - s, 2);
      const bbox = sheet.stamp(shape, tx, ty, s, t, color);
      if (bbox !== null) truth.push({ id: truth.length, shape, bbox });
    }
  }
}

/// C10: noisy scan — per-pixel background noise (one rng call per pixel),
/// icons stamped over it, isolated specks ≤ 3 px (area ≤ 9 < min_area 16).
function buildNoisy(rng, sheet, truth) {
  for (let i = 0; i < sheet.px.length; i++) {
    sheet.px[i] = 255 - rng.range(0, 13);
  }
  const n = 5;
  const rows = 4;
  const margin = 24;
  const cellw = idiv(sheet.w - 2 * margin, n);
  const cellh = idiv(sheet.h - 2 * margin, rows);
  for (let row = 0; row < rows; row++) {
    for (let col = 0; col < n; col++) {
      const s = rng.range(42, 54);
      const shape = rng.pick(SHAPES);
      const t = strokeFor(rng, shape, s);
      const color = rng.range(0, 30);
      const tx = margin + col * cellw + idiv(cellw - s, 2);
      const ty = margin + row * cellh + idiv(cellh - s, 2);
      const bbox = sheet.stamp(shape, tx, ty, s, t, color);
      if (bbox !== null) truth.push({ id: truth.length, shape, bbox });
    }
  }
  const specks = [];
  for (let i = 0; i < 14; i++) {
    const sx = rng.range(8, sheet.w - 12);
    const sy = rng.range(8, sheet.h - 12);
    const side = rng.range(1, 3);
    const ok = specks.every(([px, py]) => Math.abs(px - sx) > 16 || Math.abs(py - sy) > 16);
    if (ok) {
      specks.push([sx, sy]);
      for (let y = 0; y < side; y++) {
        for (let x = 0; x < side; x++) {
          sheet.px[(sy + y) * sheet.w + (sx + x)] = 0;
        }
      }
    }
  }
}

/// C8 palette: luma → RGB (see the Rust generator for the rationale).
const COLOUR_PALETTE = [
  [76, 255, 0, 0], // red
  [150, 0, 255, 0], // green
  [29, 0, 0, 255], // blue
  [151, 255, 128, 0], // orange
  [67, 128, 0, 255], // purple
];

function buildColour(rng, sheet, truth) {
  const n = 4;
  const margin = idiv(sheet.w, 2 * n);
  const cell = idiv(sheet.w - 2 * margin, n);
  for (let row = 0; row < n; row++) {
    for (let col = 0; col < n; col++) {
      const s = rng.range(44, 56);
      const shape = rng.pick(SHAPES);
      const t = strokeFor(rng, shape, s);
      const color = COLOUR_PALETTE[(row * n + col) % COLOUR_PALETTE.length][0];
      const tx = margin + col * cell + idiv(cell - s, 2);
      const ty = margin + row * cell + idiv(cell - s, 2);
      const bbox = sheet.stamp(shape, tx, ty, s, t, color);
      if (bbox !== null) truth.push({ id: truth.length, shape, bbox });
    }
  }
}

// ---- canonical JSON (serde_json parity: BTreeMap → sorted keys) ----

function compact(v) {
  if (Array.isArray(v)) return "[" + v.map(compact).join(",") + "]";
  if (v !== null && typeof v === "object") {
    return (
      "{" +
      Object.keys(v)
        .sort()
        .map((k) => JSON.stringify(k) + ":" + compact(v[k]))
        .join(",") +
      "}"
    );
  }
  return JSON.stringify(v);
}

function pretty(v, level = 0) {
  const pad = "  ".repeat(level);
  const padIn = "  ".repeat(level + 1);
  if (Array.isArray(v)) {
    if (v.length === 0) return "[]";
    return "[\n" + v.map((x) => padIn + pretty(x, level + 1)).join(",\n") + "\n" + pad + "]";
  }
  if (v !== null && typeof v === "object") {
    const keys = Object.keys(v).sort();
    if (keys.length === 0) return "{}";
    return (
      "{\n" +
      keys
        .map((k) => padIn + JSON.stringify(k) + ": " + pretty(v[k], level + 1))
        .join(",\n") +
      "\n" +
      pad +
      "}"
    );
  }
  return JSON.stringify(v);
}

// ---- sheet specs (mirror generate_all) ----

const specs = [
  { name: "01_basic_grid", w: 512, h: 512, bg: 255, keystone: false, output: "gray-png",
    build: (rng, sh, tr) => buildGrid(rng, sh, tr, { n: 4, sizes: [44, 52], drift: 0, colors: [0, 20] }) },
  { name: "02_mixed_grid", w: 768, h: 768, bg: 244, keystone: false, output: "gray-png",
    build: (rng, sh, tr) => buildGrid(rng, sh, tr, { n: 6, sizes: [32, 62], drift: 0, colors: [0, 40] }) },
  { name: "03_rings_holes", w: 640, h: 640, bg: 255, keystone: false, output: "gray-png",
    build: (rng, sh, tr) => {
      const margin = idiv(sh.w, 10);
      const cell = idiv(sh.w - 2 * margin, 5);
      for (let row = 0; row < 5; row++) {
        for (let col = 0; col < 5; col++) {
          const s = rng.range(56, 64);
          const shape = rng.nextU64() % 2n === 0n ? "ring" : "frame";
          const t = rng.range(10, 14);
          const tx = margin + col * cell + idiv(cell - s, 2);
          const ty = margin + row * cell + idiv(cell - s, 2);
          const bbox = sh.stamp(shape, tx, ty, s, t, 0);
          if (bbox !== null) tr.push({ id: tr.length, shape, bbox });
        }
      }
    } },
  { name: "04_scattered", w: 768, h: 768, bg: 250, keystone: false, output: "gray-png",
    build: (rng, sh, tr) => buildScattered(rng, sh, tr, 30, [36, 56], 26) },
  { name: "05_dense_grid", w: 896, h: 896, bg: 255, keystone: false, output: "gray-png",
    build: (rng, sh, tr) => buildGrid(rng, sh, tr, { n: 8, sizes: [56, 64], drift: 0, colors: [0, 24] }) },
  { name: "06_thick_strokes", w: 640, h: 640, bg: 235, keystone: false, output: "gray-png",
    build: (rng, sh, tr) => {
      const margin = idiv(sh.w, 10);
      const cell = idiv(sh.w - 2 * margin, 4);
      const shapes = ["ring", "frame", "cross", "plus"];
      for (let row = 0; row < 4; row++) {
        for (let col = 0; col < 4; col++) {
          const s = rng.range(88, 96);
          const shape = shapes[(row * 4 + col) % 4];
          const t = rng.range(12, 18);
          const tx = margin + col * cell + idiv(cell - s, 2);
          const ty = margin + row * cell + idiv(cell - s, 2);
          const bbox = sh.stamp(shape, tx, ty, s, t, 0);
          if (bbox !== null) tr.push({ id: tr.length, shape, bbox });
        }
      }
    } },
  { name: "07_size_range", w: 512, h: 512, bg: 238, keystone: false, output: "gray-png",
    build: (rng, sh, tr) => {
      const sizes = [20, 28, 36, 44, 52, 64, 76, 92, 116];
      const margin = idiv(sh.w, 7);
      const cell = idiv(sh.w - 2 * margin, 3);
      for (let i = 0; i < 9; i++) {
        const row = Math.floor(i / 3);
        const col = i % 3;
        const s = sizes[i];
        const shape = SHAPES[i % SHAPES.length];
        const t = strokeFor(rng, shape, s);
        const tx = margin + col * cell + idiv(cell - s, 2);
        const ty = margin + row * cell + idiv(cell - s, 2);
        const bbox = sh.stamp(shape, tx, ty, s, t, 0);
        if (bbox !== null) tr.push({ id: tr.length, shape, bbox });
      }
    } },
  { name: "08_grid_drift", w: 768, h: 768, bg: 255, keystone: false, output: "gray-png",
    build: (rng, sh, tr) => buildGrid(rng, sh, tr, { n: 5, sizes: [48, 60], drift: 10, colors: [0, 30] }) },
  { name: "09_near_touching", w: 640, h: 640, bg: 255, keystone: false, output: "gray-png",
    build: (rng, sh, tr) => {
      const n = 5;
      const mrows = 4;
      const margin = 16;
      const cellw = idiv(sh.w - 2 * margin, n);
      const cellh = idiv(sh.h - 2 * margin, mrows);
      for (let row = 0; row < mrows; row++) {
        for (let col = 0; col < n; col++) {
          const size = Math.max((cellw < cellh ? cellw : cellh) - rng.rangeI(4, 6), 32);
          const shape = rng.pick(SHAPES);
          const t = Math.max(rng.range(4, 6), 2);
          const tx = margin + col * cellw + idiv(cellw - size, 2);
          const ty = margin + row * cellh + idiv(cellh - size, 2);
          const bbox = sh.stamp(shape, tx, ty, size, t, 0);
          if (bbox !== null) tr.push({ id: tr.length, shape, bbox });
        }
      }
    } },
  { name: "10_edge_corner", w: 640, h: 640, bg: 255, keystone: false, output: "gray-png",
    build: (rng, sh, tr) => buildEdgeCorner(rng, sh, tr) },
  { name: "11_c1_batch_grid", w: 4096, h: 4096, bg: 255, keystone: false, output: "gray-png",
    build: (rng, sh, tr) => buildGrid(rng, sh, tr, { n: 32, sizes: [40, 56], drift: 0, colors: [0, 24] }) },
  { name: "12_c2_latency_grid", w: 4096, h: 4096, bg: 255, keystone: true, output: "gray-png",
    build: (rng, sh, tr) => buildGrid(rng, sh, tr, { n: 10, sizes: [120, 150], drift: 0, colors: [0, 24] }) },
  { name: "13_c7_jpeg_grid", w: 768, h: 768, bg: 247, keystone: false, output: "jpeg",
    build: (rng, sh, tr) => buildGrid(rng, sh, tr, { n: 5, sizes: [48, 60], drift: 0, colors: [0, 30] }) },
  { name: "14_c8_colour_icons", w: 512, h: 512, bg: 255, keystone: false, output: "rgb-png",
    build: (rng, sh, tr) => buildColour(rng, sh, tr) },
  { name: "15_c9_duplicates", w: 512, h: 512, bg: 255, keystone: false, output: "gray-png",
    build: (rng, sh, tr) => buildDuplicates(rng, sh, tr) },
  { name: "16_c10_noisy_scan", w: 640, h: 640, bg: 255, keystone: false, output: "gray-png",
    build: (rng, sh, tr) => buildNoisy(rng, sh, tr) },
];

function generateSheet(spec) {
  const seed = 0x15a7n ^ ((BigInt(spec.name.length) * 0x9e3779b97f4a7c15n) & MASK64);
  const rng = new Rng(seed);
  const sheet = new Sheet(spec.w, spec.h, spec.bg);
  const truth = [];
  spec.build(rng, sheet, truth);

  const icons = truth.map((t) => ({ id: t.id, shape: t.shape, bbox: [t.bbox.x, t.bbox.y, t.bbox.w, t.bbox.h] }));
  const truthJson = {
    file: spec.name + (spec.output === "jpeg" ? ".jpg" : ".png"),
    width: spec.w,
    height: spec.h,
    expected_groups: truth.length,
    icons,
  };
  return { spec, sheet, truth, truthJson };
}

export function generateAll(outDir) {
  fs.mkdirSync(outDir, { recursive: true });
  const manifestSheets = [];
  let total = 0;
  for (const spec of specs) {
    const { sheet, truth, truthJson } = generateSheet(spec);
    const file = spec.name + (spec.output === "jpeg" ? ".jpg" : ".png");
    const imagePath = path.join(outDir, file);
    if (spec.output === "jpeg") {
      encodeJpegGray(imagePath, spec.w, spec.h, sheet.px, 85);
    } else if (spec.output === "rgb-png") {
      const rgb = new Uint8Array(spec.w * spec.h * 3);
      for (let i = 0; i < sheet.px.length; i++) {
        const entry = COLOUR_PALETTE.find(([luma]) => luma === sheet.px[i]);
        const [r, g, b] = entry ? [entry[1], entry[2], entry[3]] : [sheet.px[i], sheet.px[i], sheet.px[i]];
        rgb[i * 3] = r;
        rgb[i * 3 + 1] = g;
        rgb[i * 3 + 2] = b;
      }
      encodePng(imagePath, spec.w, spec.h, 2, rgb);
    } else {
      encodePng(imagePath, spec.w, spec.h, 0, sheet.px);
    }
    fs.writeFileSync(path.join(outDir, `${spec.name}.json`), compact(truthJson));
    const entry = {
      file,
      truth: `${spec.name}.json`,
      width: spec.w,
      height: spec.h,
      expected_groups: truth.length,
    };
    if (spec.keystone) entry.keystone = true;
    manifestSheets.push(entry);
    total += truth.length;
    console.log(`generated ${spec.name.padEnd(22)} ${spec.w}x${spec.h} icons=${String(truth.length).padStart(4)} bytes=${fs.statSync(imagePath).size}`);
  }
  const manifest = {
    corpus: "icon-forge/bench",
    version: "0.1.0",
    generated_by: "isg-gen-corpus (deterministic, fixed seed, integer rasterization)",
    sheets: manifestSheets,
    total_icons: total,
  };
  fs.writeFileSync(path.join(outDir, "manifest.json"), pretty(manifest));
  console.log(`total icons: ${total}`);
  return total;
}

function main() {
  const args = process.argv.slice(2);
  let out = path.join(ROOT, "bench", "corpus");
  let i = 0;
  while (i < args.length) {
    if (args[i] === "--out" && i + 1 < args.length) {
      out = path.resolve(args[i + 1]);
      i += 2;
    } else {
      i += 1;
    }
  }
  generateAll(out);
}

if (import.meta.url === `file://${process.argv[1]}` || process.argv[1].endsWith("gen-corpus.mjs")) {
  main();
}
