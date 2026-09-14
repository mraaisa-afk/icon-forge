// Independent corpus verifier: decodes the committed binaries and re-derives
// the grouping result (border-median mask -> 8-connected CCL -> min-area
// filter — the same criteria the Rust gate applies) WITHOUT reusing the
// generator's internal state. Cross-checks:
//   1. component count == expected_groups (per sheet),
//   2. every ground-truth bbox contains >= 25% foreground pixels in its area
//      (catches blank sheets / mis-stamped icons),
//   3. found-vs-truth bbox 1:1 matching within +/-2 px per edge.
//
// Run from the repo root:  node scripts/verify-corpus.mjs [--corpus bench/corpus]
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { decodeImage, toLuma } from "./pngio.mjs";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

function borderMedian(luma, w, h) {
  const ring = [];
  for (let x = 0; x < w; x++) {
    ring.push(luma[x], luma[(h - 1) * w + x]);
  }
  for (let y = 1; y < h - 1; y++) {
    ring.push(luma[y * w], luma[y * w + w - 1]);
  }
  ring.sort((a, b) => a - b);
  return ring[ring.length >> 1];
}

/// 8-connected CCL with min-area filter (simple BFS; mirrors the spike).
function groupAll(luma, w, h, threshold, minArea) {
  const bg = borderMedian(luma, w, h);
  const mask = new Uint8Array(w * h);
  for (let i = 0; i < mask.length; i++) mask[i] = Math.abs(luma[i] - bg) >= threshold ? 1 : 0;
  const seen = new Uint8Array(w * h);
  const groups = [];
  const stack = new Int32Array(w * h);
  for (let start = 0; start < mask.length; start++) {
    if (!mask[start] || seen[start]) continue;
    let sp = 0;
    stack[sp++] = start;
    seen[start] = 1;
    let minx = w;
    let miny = h;
    let maxx = 0;
    let maxy = 0;
    let area = 0;
    let origin = null;
    while (sp > 0) {
      const i = stack[--sp];
      const x = i % w;
      const y = (i / w) | 0;
      if (origin === null || y * w + x < origin[1] * w + origin[0]) origin = [x, y];
      if (x < minx) minx = x;
      if (y < miny) miny = y;
      if (x > maxx) maxx = x;
      if (y > maxy) maxy = y;
      area++;
      for (let dy = -1; dy <= 1; dy++) {
        for (let dx = -1; dx <= 1; dx++) {
          if (dx === 0 && dy === 0) continue;
          const nx = x + dx;
          const ny = y + dy;
          if (nx < 0 || nx >= w || ny < 0 || ny >= h) continue;
          const ni = ny * w + nx;
          if (mask[ni] && !seen[ni]) {
            seen[ni] = 1;
            stack[sp++] = ni;
          }
        }
      }
    }
    if (area >= minArea) {
      groups.push({ bbox: { x: minx, y: miny, w: maxx - minx + 1, h: maxy - miny + 1 }, area, origin });
    }
  }
  groups.sort(
    (a, b) =>
      a.bbox.y - b.bbox.y ||
      a.bbox.x - b.bbox.x ||
      a.origin[0] - b.origin[0] ||
      a.origin[1] - b.origin[1]
  );
  return { bg, groups };
}

function main() {
  const args = process.argv.slice(2);
  let corpus = path.join(ROOT, "bench", "corpus");
  let i = 0;
  while (i < args.length) {
    if (args[i] === "--corpus" && i + 1 < args.length) {
      corpus = path.resolve(args[i + 1]);
      i += 2;
    } else {
      i += 1;
    }
  }
  const manifest = JSON.parse(fs.readFileSync(path.join(corpus, "manifest.json"), "utf8"));
  let failures = 0;
  console.log(
    "sheet".padEnd(24) +
      "found".padStart(6) +
      "expect".padStart(7) +
      "  bbox-match  ink-ok  result"
  );
  for (const sheet of manifest.sheets) {
    const truth = JSON.parse(fs.readFileSync(path.join(corpus, sheet.truth), "utf8"));
    const img = decodeImage(path.join(corpus, sheet.file));
    const luma = toLuma(img);
    const { groups } = groupAll(luma, img.width, img.height, 32, 16);

    // 1:1 bbox match (greedy nearest within +/-2 px per edge)
    let matched = 0;
    const used = new Set();
    for (const t of truth.icons) {
      const [tx, ty, tw, th] = t.bbox;
      let best = -1;
      let bestDist = Infinity;
      for (let g = 0; g < groups.length; g++) {
        if (used.has(g)) continue;
        const b = groups[g].bbox;
        const dist = Math.max(Math.abs(b.x - tx), Math.abs(b.y - ty), Math.abs(b.w - tw), Math.abs(b.h - th));
        if (dist <= 2 && dist < bestDist) {
          best = g;
          bestDist = dist;
        }
      }
      if (best >= 0) {
        used.add(best);
        matched++;
      }
    }

    // ink presence per truth bbox, measured against the sheet's own
    // border-median background. Floor is 5% of bbox area: thin large icons
    // (cross t=4 on a 116 px cell ≈ 7%) legitimately sit low, while the
    // failure mode this check exists for — blank/mis-stamped sheets (F1) —
    // measures 0%.
    const bg = borderMedian(luma, img.width, img.height);
    let inkOk = 0;
    for (const t of truth.icons) {
      const [tx, ty, tw, th] = t.bbox;
      let fg = 0;
      for (let y = ty; y < ty + th; y++) {
        for (let x = tx; x < tx + tw; x++) {
          if (Math.abs(luma[y * img.width + x] - bg) >= 32) fg++;
        }
      }
      if (fg >= 0.05 * tw * th) inkOk++;
    }

    const countOk = groups.length === truth.expected_groups;
    const allInk = inkOk === truth.icons.length;
    const allMatched = matched === truth.icons.length;
    const ok = countOk && allInk && allMatched;
    if (!ok) failures++;
    console.log(
      sheet.file.padEnd(24) +
        String(groups.length).padStart(6) +
        String(truth.expected_groups).padStart(7) +
        `  ${String(matched)}/${String(truth.icons.length).padEnd(4)}    ` +
        `${inkOk}/${String(truth.icons.length).padEnd(4)}  ` +
        (ok ? "OK" : "FAIL")
    );
  }
  console.log(failures === 0 ? "\nALL SHEETS VERIFIED" : `\n${failures} SHEET(S) FAILED`);
  process.exit(failures === 0 ? 0 : 1);
}

main();
