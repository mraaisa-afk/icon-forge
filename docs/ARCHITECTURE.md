# Icon Forge — Architecture Guide

**Version:** 1.0
**Status:** Approved for implementation
**Target platform (v1):** Windows 10/11 desktop, offline, USB-distributable

---

## 0. Purpose & Scope

Icon Forge is an offline desktop application that:

1. **Auto-vectorizes** raster icon sheets (PNG/JPEG containing dozens–hundreds of small icons) into clean, editable SVGs.
2. **Auto-groups** scattered or gridded icons into individual, correctly-bounded units.
3. **Auto-levels** the visual size of icons so they read as a consistent set (compensating for stroke weight, solidity, etc.).
4. Provides an **SVG editor** and a **review/triage workflow** so a human can fix, approve, or reject results fast at scale (hundreds to thousands of icons).
5. Exports finished icon sheets with metadata (grid, CSV manifest, multiple formats).

Non-goals for v1: cloud sync, multi-user collaboration, mobile builds, non-Latin font/glyph tracing.

---

## 1. Tech Stack

### 1.1 Decision matrix

| Layer | Choice | Why |
|---|---|---|
| Shell | **Tauri 2.x** | 3–10 MB installer vs Electron's 90–200 MB; ~50 MB idle RAM; small offline installer matches the "100% offline, USB-distributable" requirement |
| Core language | **Rust** | `rayon` work-stealing parallelism; explicit memory control (bit-packed 1-bpp masks instead of 64 MB RGBA buffers) |
| Vectorization | **vtracer 1.x (MIT)** | Rust-native, MIT-licensed, pluggable framework: BW + colour clustering, adaptive Bradley–Roth thresholding, `polygon`/`spline`/`pixel` fit modes, `stacked`/`cutout` hierarchy, fixed palettes, separable `segment()`/`finish()` stages (cache segmentation, re-run only curve fitting) |
| Image ops | **imageproc** (MIT) + **rten-imageproc** (MIT/Apache) | morphology, Otsu/adaptive threshold, connected-component labeling (CCL), distance transform, `find_contours` |
| Render | **resvg + tiny-skia** (MPL-2.0) | Pure-Rust SVG→raster for previews, PNG export, visual diffs; no system deps |
| Geometry | **kurbo** | beziers, arcs, affine transforms, flattening |
| Storage | **rusqlite** (bundled SQLite, WAL) | 10k+ assets with review state, no external DB process |
| UI | **React 18 + TypeScript + Vite + Zustand + Radix + Tailwind** | standard, fast, well-supported |
| Editor canvas | **Raw Canvas2D + `Path2D`** (NOT Konva/Fabric) | `Path2D` parses SVG `d` strings natively; `isPointInPath` gives exact hit-testing incl. `fill-rule`. Konva/Fabric have historical gaps with elliptical arcs and `evenodd` — exactly what a vectorizer emits. ~800 LOC of custom scene graph beats fighting a library. |
| Booleans | `polygon-clipping` (ISC) + `svg-path-commander` (MIT) | path boolean ops for the editor |

### 1.2 Keystone architectural decision

**Compile `isg-core` twice: native and WASM.**

- One Rust crate with **zero platform dependencies** (no `tauri`, no `std::fs`, no threads).
- Linked into the Tauri binary for batch/native work (runs under `rayon`).
- Built with `wasm-pack` for the webview, for interactive geometry (hit-test, snap, boolean ops, auto-level) — runs in-browser at <1 ms with no IPC round-trip.
- **CI enforcement:** `cargo check --target wasm32-unknown-unknown -p isg-core` must pass on every commit. If this fails, the platform boundary has been violated (e.g. someone added a `std::fs` call to `isg-core`).

### 1.3 Licensing rules (hard constraints)

- **Never use potrace or autotrace** — both GPL-2.0, would force open-sourcing the entire app.
- **resvg (MPL-2.0)** is file-level copyleft: use it unmodified, behind an adapter module; the only obligation is a notice.
- Add **`cargo deny`** to CI with an explicit allow-list and a hard GPL/AGPL ban. Fail the build on any disallowed license.

### 1.4 Fallback path (only if no Rust experience on the team)

Electron + Python sidecar (OpenCV + `vtracer` from PyPI) can produce a working demo in 2–3 weeks (`cv2.findContours` + `cv2.connectedComponents` map ~1:1 onto §3 below). Treat this only as a **throwaway Phase-0 prototype** for tuning thresholds with a human in the loop — the GIL, a 250 MB+ bundle, and two runtimes make the 2-second / 1000-image targets fragile long-term. Port the hot core to Rust before Phase 2.

---

## 2. System Architecture

```
┌─────────────────────────────────────────────────────────────┐
│  Tauri Shell (Rust host process)                             │
│  ┌───────────────┐   ┌───────────────────────────────────┐   │
│  │  isg-core      │   │  Job Engine (T0/T1/T2 tasks,      │   │
│  │  (native)      │──▶│  cancellation, preemption)         │   │
│  │  batch work    │   └───────────────────────────────────┘   │
│  └───────────────┘   ┌───────────────────────────────────┐   │
│                       │  rusqlite (WAL) — project state,   │   │
│                       │  review flags, cache index         │   │
│                       └───────────────────────────────────┘   │
└───────────────────────────┬───────────────────────────────────┘
                             │ IPC (Tauri commands/events)
┌───────────────────────────▼───────────────────────────────────┐
│  Webview (React 18 + TS)                                       │
│  ┌───────────────┐   ┌────────────────────────────────────┐   │
│  │  isg-core      │   │  Canvas2D/Path2D editor, virtualized│   │
│  │  (WASM)        │──▶│  library grid, review workspace      │   │
│  │  interactive   │   └────────────────────────────────────┘   │
│  └───────────────┘                                              │
└──────────────────────────────────────────────────────────────┘
```

**Golden rule:** interactive, per-frame work (drag, snap, hit-test, auto-level preview) never crosses the IPC boundary — it runs in WASM. Only batch jobs (import, full-sheet vectorization, export) go through the native job engine.

---

## 3. Core Algorithms

### 3.1 The one decision that makes "2 seconds" possible

**Segment first, trace second.** Never vectorize the whole multi-icon sheet and split vectors afterward.

```
raster → foreground mask → connected components → per-icon CROPS → trace each crop in parallel
```

Each crop is ~200×200 px: tracing is milliseconds, crops are embarrassingly parallel, each icon gets its own local origin, and the background frame is never traced.

### 3.2 Latency budget (reference target: 4096×4096, 100 icons, 8 cores)

| Stage | Cost (parallel) |
|---|---|
| Decode + RGB→gray (rayon rows) | 60 ms |
| Downscale to 1536 analysis res | 20 ms |
| Background detect + binarize (Otsu on ΔE-Lab) | 12 ms |
| Morphology open+close (bit-packed, separable) | 15 ms |
| CCL on RLE runs (two-pass union-find) | 10 ms |
| Component stats | 6 ms |
| Fragment merge (spatial hash) | 2 ms |
| Watershed split (flagged components only) | 25 ms |
| Grid detection + containment tree | 6 ms |
| Trace 100 crops (vtracer ‖ rayon) | 45 ms |
| Path optimize + emit + metrics | 27 ms |
| **Total** | **≈ 230 ms** |

That's an ~8× margin under the 2 s target. Naive implementations blow the budget by tracing the whole image (10–60 s), keeping RGBA8 per worker (512 MB), and skipping the containment tree (holes become separate icons).

### 3.3 Auto-Vectorization pipeline (8 stages)

**① Normalize** — EXIF orientation, CMYK/16-bit/palette → RGBA8, cap max dimension. Fix the resampling filter for determinism (same input → byte-identical output).

**② Background detection** — border-consensus histogram (4-4-4 bit bins); if >85% of border pixels agree, that's the background. Build the mask with **CIE-Lab ΔE**, not RGB distance (JPEG artifacts are RGB outliers but perceptually identical to the background; ΔE cuts speckle sharply). Fallback order: alpha channel → k=2 k-means seeded from corners.

**③ Clean** — `median(3)` → `open(3)` → `close(3)`. The close pass is critical: an unclosed "C" becomes two icons downstream. For JPEG quality <80, widen to `close(5)`.

**④ Quantize** (colour mode only) — deterministic k-means++, elbow-method k, de-fringing open per layer, layers ordered by ink area DESC = paint order. Pick a hierarchy strategy:
- **stacked** — flat icons, no holes
- **cutout** — abutting regions, seam-free (`vtracer::Hierarchical::Cutout`)
- **hole-aware** — containment tree → `evenodd` subpaths
- **pixel** — RLE-merge runs, `FitMode::Pixel`, no curves

**⑤ Trace** — vtracer per crop. Ship 7 presets mapped to a 1–5 Quality slider: `mono-fast`, `mono-clean` (BW + adaptive — default for B/W sets), `flat-8`, `flat-cutout`, `detailed`, `pixel-art`, `scan`.

**⑥ Simplify** — this stage is custom (not vtracer's), and is what makes icons survive at 16 px:
```
abs-cubics → remove collinear (0.5°) → RDP with CORNERS PINNED
           → Visvalingam (area-based, kills curve jitter) → axis-snap within 0.15px
```
Two passes because RDP is distance-based (good for straight segments) and Visvalingam is area-based (good for jitter). Corner pinning preserves arrow tips and star points.

**⑦ Emit + validate** — merge same-style paths, quantize coordinates, normalize viewBox, write `<title>/<desc>/<metadata>`. Then **re-parse with `usvg`**; if it fails, flag and fall back. **Never write an unparseable SVG.**

**⑧ Score + cache** — render with resvg, compute MAE + SSIM + IoU on ink-centroid-aligned alpha channels → composite score. Cache key: `blake3(bytes) ‖ preset ‖ segParams ‖ version`, zstd on disk.

### 3.4 Auto-Grouping ("Group All")

Design against five failure modes, in this order.

**Mask → RLE.** Represent the mask as run-length-encoded rows (`{y, x_start, x_end}`). At 5% ink coverage a 4096² sheet is ~50–200 KB instead of 64 MB. **All subsequent steps run on runs, not pixels** — this is the single biggest speed lever.

**CCL (8-connectivity).** Two-pass union-find over runs; a run at row `y` unions with any run at `y-1` overlapping `[x_start-1, x_end+1]` (the ±1 gives 8-connectivity). O(R·α(R)) ≈ 10 ms.

**F4 Noise — filter before merging** (or merge cost explodes). Drop area < 16 px, min dim < 3, aspect > 60, and anything spanning ≥2 opposite borders at >90% coverage (frame/rule lines). Then compute `median_h` and `median_area` — every threshold downstream derives from these, no magic constants.

**F1 Fragmentation — merge.** `GAP = clamp(0.35 × median_h, 4px, 40px)`. Spatial-hash the bboxes, then five rules:
1. small piece near big piece within GAP → merge
2. **refuse if merged aspect > 4.0**
3. **refuse if merged height > 1.9 × median_h**
4. IoU > 0.25 or containment → merge
5. within 0.5×GAP, height ratio ∈ [0.6, 1.6], **and combined area ≤ 1.75 × median_area** → merge

Max 3 iterations. Rules 2 and 3 exist specifically to stop the classic cascade that glues a whole row into one blob.

Rule 5's area guard exists because rules 2 and 3 alone do not stop that cascade once the pieces are small: without it, a row of equal-sized icons with 4 px gaps glues into one blob (each pair passes the gap and height-ratio tests while the merged box stays under the aspect and height ceilings) and no exact-count gate can survive it. With it, `merge_refuses_two_full_size_icons_at_a_four_pixel_gap` holds 12 → 12 while body + fragment still merges (`merge_joins_a_small_fragment_within_gap`). Both tests are the evidence anchor for this rule.

**F2 Over-merge — split.** Only for components whose bbox area exceeds `(2.2 × median_h)²`: chamfer distance transform → non-max-suppressed local maxima as seeds (radius 0.6 × median_h, peaks at least `0.3 × median_h` from background) → **Meyer's marker-controlled watershed** with a binary heap → reject slivers under 10% of expected, reattaching each sliver to the adjacent region with the longest shared boundary (ties → lowest label). Two deterministic fallbacks when seeding disagrees with the bbox aspect:

* **under-seeded** (fewer than 2 peaks while the bbox holds ≥ 2 median-sized icons) → cut the component into `clamp(round(bbox_area / median_h²), 2, max_seeds)` bands at the lowest-ink profile positions of the long axis (ties → lowest coordinate, minimum band width 2 px);
* **over-seeded** (more peaks than `round(bbox_area / median_h²)`, the count of median-sized icons that fit in the bbox) → keep the strongest peaks by (peak distance desc, scan index asc) and re-run the watershed.

Both fallbacks compare seeding against the same quantity — how many `median_h`-sized cells fit in the component's bbox — because that is the only count that stays meaningful for 2-D glue (a row *and* a grid), which the long-axis count alone does not.

A component whose inscribed radius (`dt_max`) exceeds `0.75 × median_h` is **never** split: it is one large icon, not a merge, and this guard is what keeps a legitimately solid large icon out of the splitter. Determinism is by construction — every ordering (seed peaks, heap pops, sliver reattachment, band cuts) breaks ties on scan index — and a split stands only when ≥ 2 regions survive sliver rejection. Evidence anchors: `split_candidates_are_size_gated`, `splits_a_glued_grid_into_cells`, `slivers_are_reattached_to_the_longest_border`, `under_seeding_falls_back_to_profile_cuts`, `over_seeding_falls_back_to_reseeding`, `fat_component_is_never_split`.

**F3 Holes.** Sort by area DESC, sweep an x-interval active list, build the containment forest. Containment is strict bbox containment (equal boxes never nest; the smallest candidate area wins) and is *verified*, not assumed: flood the outer crop's background from its border — the inner group's own pixels must stay unreachable (two diagonal neighbours share bbox area all the time), and a background pocket counts as a hole only when it touches the outer group's **own** ink, because a group's bbox may share ink with a neighbour and a nested ring's pocket belongs to the child. **Even-odd depth parity: depth 0 = body, the enclosing hole level = 2·depth − 1, an inner icon = 2·depth.** A ring or the letter "O" is one icon, not two. Holes attach as `evenodd` subpaths at trace time.

**F5 Grid drift.** Projection profiles of ink row/column sums → interior empty runs are valleys (a zero run that reaches the sheet border is a margin, not a separator) → if regularity (1 − MAD/median of the valley pitch) > 0.75 with ≥ 2 valleys on that axis, treat it as a grid. A component whose bbox straddles a valley centre (± 2 px) is flagged `SpansMultipleCells` and sent back for splitting. *(Amended in W10: the earlier "fits inside one cell inflated by −2px" test was measured on the frozen corpus and rejected — with real 35–40 px valleys it flags 13/20 icons on `09_near_touching` and 60/64 on `05_dense_grid`. Straddle + regularity flags **nothing** on any of the 15 PNG sheets while still flagging all three provable F1 merges on `09_near_touching`.)* A flagged component produced by an F1 proximity merge cannot be re-split by the watershed (the merge is disjoint by construction, so the crop-scoped splitter sees a single region): the merge pass keeps its provenance (`members`) and F5 restores those **original components verbatim**; only a single-component flag (real glue) goes to the watershed, with the F2 size gate skipped because the hint is the evidence the gate stood in for. **Grid detection is a hint layer, never authoritative** — this is what lets one algorithm handle both neat sprite sheets and scattered layouts.

**Confidence score** with weighted deductions:

| Signal | Deduction |
|---|---|
| Spans multiple cells | −0.30 |
| High merge ratio | −0.20 |
| Size MAD (outlier spread) | −0.15 |
| Border touch | −0.15 |
| Weak grid | −0.10 |
| Uncertain background | −0.10 |

Surfaced to the user as: *"Grouped 84 icons in 0.31 s · confidence 92% · 3 groups need review."*

**Override path** (no segmentation is ever 100%): segmentation overlay (coloured labels + bboxes + warnings), marquee "Group Selected", a "Split Here" line tool that re-runs CCL on one component in <20 ms, and sensitivity sliders that re-run from the **cached mask** without re-decoding (~50–200 ms, feels live).

**Confidence as built (W11).** `confidence.rs` computes `1 − Σ weightᵢ·signalᵢ` with every signal in `[0, 1]`, so the weights above stay the spec's and only the ramps are calibration: spans = share of groups the F5 lattice check acted on (groups still straddling a valley **plus** the originals F5 restored from provably wrong merges — a sheet that needed three corrections must not read as 100 %); merge ratio = `merges / (groups + merges)` over the F1 merges that survived those corrections (F4 dust removal is not a merge); size MAD = `MAD / median` of `min(w, h)` ramped from 0.35 to 0.70; border touch = share of groups within 2 % of a sheet border; weak grid = share of axes with ≥ 2 valleys whose *measured* regularity lands in `[0.35, 0.75)`; uncertain background = ramp below 0.85 consensus, full for the k-means/Otsu fallbacks. Warnings are per group — `spans multiple cells` or `restored from a merge across a valley` — and the review count the status line prints is their length. Measured on the frozen PNG corpus (W11): 12 of 15 sheets score exactly 1.000, `07_size_range` 0.984 (real size spread), `10_edge_corner` 0.900 (icons on the border), `09_near_touching` 0.910 (six restored groups ⇒ 0.30 spans × 0.30).

**Override path as built (W11).** `maskcache.rs` caches the segmentation result — the mask as RLE runs plus the background model, deliberately **not** the sheet raster (4096² RGBA+luma is ~134 MB and grouping never reads pixels) — in a content-addressed LRU keyed `blake3(bytes) ‖ blake3(SegParams::to_cache_string()) ‖ version`, so a different segmentation config can never hit and the same one can never miss. `pipeline::mask_cached` asks the cache first and only calls `segment` on a miss, i.e. a warm slider tick performs **no decode and no segmentation**; `regroup_cached` then runs RLE CCL + the refine chain (and the confidence score, with the cached background) off that mask. Measured on the 4096² sheets: `12_c2_latency_grid` cold 0.86 s → warm 25 ms (33×, identical groups), `11_c1_batch_grid` cold 0.87 s → warm 52 ms (17×) — inside this section's "~50–200 ms, feels live" target.

**Override path as built (W12).** `pipeline::grouping` owns the stateful session behind the overlay — one mask cache plus the sheet on screen — so the UI never re-derives anything. `GroupingSession::group_sheet` runs RLE CCL → the refine chain → the confidence score off the **cached** mask, and remembers that sheet's hint and F5 restorations; `set_refine` (the sliders) swaps refine parameters and re-groups from the same mask, i.e. no decode and no segmentation.

Three deliberate as-built decisions, because the code must not silently outgrow this section:

* **Split Here uses the F5 path, not a bare CCL re-run.** A click is the *evidence* the F2 size gate stood in for, so the clicked group goes through `resplit_forced` (the W9 watershed with the size gate skipped); every other guard still applies — the fat `dt_max` ceiling, ≥ 2 surviving regions, sliver reattachment, scan-index tie-breaks — so a solid icon is never cut and a refusal leaves the list untouched.
* **Manual edits are view state, not a new segmentation.** `group_selected` collapses the marquee's groups into one (union box, summed area, first member's origin); the list is then **re-scored** with the hint and restorations the chain measured, so an edit never erases the warnings that were on screen — a marquee union across a lattice comes back flagged. `reset_manual` (what "Group All" does again) drops every edit and restores the automatic result exactly.
* **Sensitivity has an envelope.** `SensitivityParams` maps four sliders onto four documented parameters (F1 `gap_frac`, rule 5 `rule5_max_area_ratio`, F4 `noise_min_area`, F5 `regularity_min`) and **rejects** anything outside `RANGES` instead of clamping silently; the UI clamps to the same numbers (`SENSITIVITY_SLIDERS`), so a slider can never produce a request the backend refuses.

The overlay backdrop is `sheet_preview`: `SheetRaster::scaled_rgba` box-averages the normalized sheet (integer half-up rounding ⇒ byte-deterministic, cached per sheet+scale in the session) and the command base64-encodes the PNG with the sheet dimensions, so boxes drawn at sheet coordinates land exactly on their icons. Evidence for this work item lives in `tests/phase3_gate.rs` (C2 median-of-5 cold runs, C4, C5, Split Here budget, Group Selected, slider regroup, preview) and in `crates/isg-native/src/pipeline/grouping.rs`'s unit tests over synthetic masks.

### 3.5 Auto Leveling

```
target_ink = (cell.w − 2·padding) · ink_ratio          # e.g. (64−16)·0.80 = 38.4px
s_fit    = min(target_ink/ib.w, target_ink/ib.h)
s_stroke = clamp((median_stroke / icon.stroke_weight)^0.35, 0.88, 1.12)   ← secret sauce
s_solid  = clamp(1 + 0.06·(median_solidity − icon.solidity), 0.96, 1.04)
```

A thin-outline icon scaled to the same bbox as a solid icon *looks smaller*. Stroke-weight compensation fixes it — but naive linear compensation makes thin icons balloon, hence the `^0.35` damping plus hard clamps.

Placement options: **Center** / **OpticalCenter** (shift by 50% of the bbox-center-to-area-centroid delta — corrects top-heavy arrows and pins) / **Baseline**, with an overflow guard that backs off to pure fit and flags the icon.

Stroke weight itself is derived from `2 × max_inscribed_radius` via the distance transform.

Because this is pure arithmetic on cached metrics, it runs at **1000 icons in <20 ms, in WASM, live on every slider drag**. Report `ink_size_cv` (target <0.05), `stroke_cv`, and baseline spread so results are explainable to a designer.

### 3.6 Review system — three detectors

1. **Quality** — resvg render at 2× cell → MAE/SSIM/IoU → auto-flag `LowQuality` below 0.80, `OverComplex` when node count > 4·√area, `OpenContour`.
2. **Duplicates (3-stage cascade)** — dHash+aHash on normalized 64×64 renders with LSH banding (O(n), not O(n²)) → verify with mask IoU ≥ 0.92 or normalized Hausdorff ≤ 2% → confirm with blake3/SSIM ≥ 0.97. Suggests a keeper per cluster.
3. **Outliers** — MAD-based modified z-score > 3.5 on ink size, stroke, node count, colour count, solidity — plus modal style/palette mismatch. Catches the real-world defect: *"99 icons are 2px outline, one is a filled blob."*

Plus keyboard triage: `A`pprove / `R`eject / `F`lag / `D`uplicate / `Space` overlay / `Shift+A` bulk — target 1000 icons in 15 minutes, every decision an undoable, timestamped command, exported as `review.csv`.

### 3.7 The SVG editor as built (4A)

**The document model is `isg-core::editor`** — `Doc` (nodes with an affine, fill, visibility and a path of line/cubic subpaths), `Command` (translate, scale, rotate, centre, fill, visibility, reorder, duplicate, delete) and a bounded `History` that stores each step's forward *and* inverse node operations. Two properties are load-bearing and tested: integers are stored exactly (inverse ops are built pointwise beside their forward op and applied in reverse, so index operations undo exactly), and the selection is canonicalised to z-order after every operation, which is what makes `redo(do(x))` produce the same state as `do(x)` rather than the same nodes in a different selection order. This is the one deliberate exception to the frozen-crate rule of §1.2: editing geometry is the same geometry the tracer emits, so both live in the crate that compiles for `wasm32` — see the note under §7.

**The binding is hand-rolled, not `wasm-pack`/`wasm-bindgen`** (a deviation from §1.1, recorded here deliberately). `crates/isg-wasm` exports six `#[no_mangle] extern "C"` functions (`editor_call(feature, a, b)`, `editor_abi_version`, and the four table accessors) and owns two tables inside its own linear memory: 1 Mi words of input, 256 Ki words of output. Geometry travels as `f32` bit patterns through `u32` words, and text as UTF-8 packed four bytes per word; `ABI_VERSION` is 1. The rules that make it safe to hand the host raw words are:

* **Every id list and node-record list ends with a zero word.** Ids are never 0, so a host can walk a list safely even when the output table still holds words from an earlier call — the bug that a "count records, then read" host hits.
* **`ERROR` reports the most recent call**, and a failed call returns `0`; `ERR_*` codes 1–10 are a 1:1 map of `CommandError`, so a refusal is a code the UI can print, never a panic and never a silent no-op.
* **The document blob is length-checked before use** (a 4-word header carrying its own word count, then a 10-word node header and a self-describing path blob), and a malformed or zero-sized blob is rejected rather than decoded into a 0×0 document.
* The tables grow linear memory, which **detaches** typed views a host may have cached: the TypeScript adapter re-derives its views after every call, and the canvas walks the live buffer without caching one.

**Rendering is Canvas2D as specified (§1.1), with two measured deviations.** Hit-testing runs in the engine (`PICK`/`MARQUEE`) rather than via `isPointInPath`, because the editor must agree with the geometry it stores, and the per-frame draw walks the output table through a `Float32Array` view with no per-node allocation rather than building `Path2D` objects: on a synthetic 1000-icon sheet (5000 paths, 15 000 segments) the allocation-free walk keeps the geometry pass under a frame's 16.6 ms budget, while decoding the same records into JavaScript objects costs ~4 µs per segment and blows the frame. The measured numbers are printed by the tests on every CI run.

**SVG parsing is on the TypeScript side for 4A** (a deviation from §2's "geometry work in Rust"): the tracer already produces SVG, so `src/wasm/svgPath.ts` converts its `M/L/H/V/C/Z` output into node geometry, and refuses anything else (quadratic, arc, smooth variants) instead of guessing. Moving icon tracing behind the ABI — one call that turns a sheet bbox into document nodes — is 4B/4C work; the adapter's API does not change when it lands.

**The Phase 4 exit criterion is checked in three places**, all on the real code path: the host-side ABI walk (4000 randomised sequences, inverted through `UNDO`/`REDO`), the module-table walk (600 iterations, through the exact entry point `editor_call` forwards to), and the TypeScript adapter walk (1000 iterations, through the same byte protocol the browser uses). The canvas half of the criterion (5000 paths @ 60 fps) is measured as the geometry pass described above, since rasterisation belongs to the webview.

---

## 4. Data Model (sketch)

```sql
-- project.isgproj (SQLite, WAL mode)

CREATE TABLE sheets (
    id            BLOB PRIMARY KEY,   -- uuid
    source_path   TEXT NOT NULL,
    content_hash  TEXT NOT NULL,      -- blake3
    width         INTEGER NOT NULL,
    height        INTEGER NOT NULL,
    imported_at   TEXT NOT NULL
);

CREATE TABLE icons (
    id            BLOB PRIMARY KEY,
    sheet_id      BLOB NOT NULL REFERENCES sheets(id),
    bbox_x        INTEGER NOT NULL,
    bbox_y        INTEGER NOT NULL,
    bbox_w        INTEGER NOT NULL,
    bbox_h        INTEGER NOT NULL,
    svg_path      TEXT,               -- relative path to cached SVG
    preset        TEXT,
    quality_mae   REAL,
    quality_ssim  REAL,
    quality_iou   REAL,
    confidence    REAL,
    group_id      BLOB,
    review_state  TEXT DEFAULT 'pending', -- pending|approved|rejected|flagged|duplicate
    reviewed_at   TEXT,
    stroke_weight REAL,
    solidity      REAL,
    ink_area      INTEGER
);

CREATE TABLE cache (
    key           TEXT PRIMARY KEY,   -- blake3(bytes) || preset || segParams || version
    payload_path  TEXT NOT NULL,      -- zstd-compressed on disk
    created_at    TEXT NOT NULL
);

CREATE TABLE review_log (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    icon_id       BLOB NOT NULL,
    action        TEXT NOT NULL,      -- approve|reject|flag|duplicate|undo
    timestamp     TEXT NOT NULL
);
```

---

## 5. Repo Layout

```
icon-forge/
├── Cargo.toml                 # workspace root
├── crates/
│   ├── isg-core/               # platform-agnostic: algorithms, geometry, traits
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── vectorize/      # §3.3 pipeline stages
│   │       ├── group/          # §3.4 grouping
│   │       ├── level/          # §3.5 auto-leveling
│   │       ├── review/         # §3.6 detectors
│   │       └── geometry/       # kurbo wrappers, path ops
│   ├── isg-native/             # native-only glue (rayon, fs, sqlite)
│   │   └── Cargo.toml
│   └── isg-wasm/                # wasm-pack target, thin bindings over isg-core
│       └── Cargo.toml
├── src-tauri/                  # Tauri shell, commands, job engine
│   ├── Cargo.toml
│   ├── tauri.conf.json
│   └── src/
│       ├── main.rs
│       ├── commands/
│       └── jobs/
├── src/                         # React + TS frontend
│   ├── main.tsx
│   ├── components/
│   │   ├── library/             # virtualized icon grid
│   │   ├── editor/               # Canvas2D/Path2D SVG editor
│   │   └── review/                # triage workspace
│   ├── state/                    # Zustand stores
│   └── wasm/                      # isg-wasm bindings
├── benchmarks/                  # C1–C10 benchmark corpus + harness
├── .github/workflows/
│   ├── ci.yml                    # build, test, cargo deny, wasm check
│   └── release.yml
└── docs/
    └── ARCHITECTURE.md          # this file
```

---

## 6. Key Cargo.toml dependencies (workspace)

```toml
[workspace]
members = ["crates/isg-core", "crates/isg-native", "crates/isg-wasm", "src-tauri"]
resolver = "2"

[workspace.dependencies]
vtracer = "1"
imageproc = "0.25"
resvg = "0.44"
tiny-skia = "0.11"
usvg = "0.44"
kurbo = "0.11"
rayon = "1"
rusqlite = { version = "0.32", features = ["bundled"] }
blake3 = "1"
zstd = "0.13"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

`cargo-deny.toml` must include an explicit `allow` list (MIT, Apache-2.0, MPL-2.0, ISC, BSD-*) and `deny = ["GPL-2.0", "GPL-3.0", "AGPL-3.0"]`.

---

## 7. isg-core trait sketch (frozen at end of Phase 0)

```rust
// crates/isg-core/src/lib.rs

pub trait Vectorizer {
    fn segment(&self, img: &RasterImage) -> SegmentationResult;
    fn trace(&self, crop: &Crop, preset: TracePreset) -> Result<SvgIcon, TraceError>;
}

pub trait Grouper {
    fn group(&self, mask: &RleMask) -> GroupingResult; // includes confidence score
}

pub trait Leveler {
    fn level(&self, icons: &[IconMetrics], cell: CellSpec) -> Vec<Transform>;
}

// isg-core also owns the editor's document model, node/command types and the
// bounded undo history (`isg_core::editor`, added in Phase 4A). It is the one
// module that grew into the frozen crate after Phase 0, and deliberately so:
// the editor must transform the *same* geometry the tracer emits, and Rust is
// the only language both the native pipeline and the webview share. It keeps
// the crate's hard constraints — zero dependencies, no platform types, builds
// for wasm32-unknown-unknown with a `cargo check` — and `cargo check
// --target wasm32-unknown-unknown -p isg-core` remains the enforcement.

pub trait ReviewScorer {
    fn score_quality(&self, icon: &SvgIcon, reference: &RasterImage) -> QualityScore;
    fn find_duplicates(&self, icons: &[SvgIcon]) -> Vec<DuplicateCluster>;
    fn find_outliers(&self, icons: &[IconMetrics]) -> Vec<OutlierFlag>;
}
```

No platform types (`std::fs::File`, `tauri::*`, thread handles) may appear in any public signature in `isg-core`. This is the boundary the WASM CI check enforces.

---

## 8. Roadmap — 28 weeks

> **Golden rule: the riskiest algorithm ships first.** Phase 0 exists only to prove or disprove the "2 s / 100 icons" claim before a single line of UI is written.

| Phase | Weeks | Deliverable | Exit criteria |
|---|---|---|---|
| **0 — Foundations & Spike** | 1–2 | Benchmark corpus (C1–C10), throwaway segmentation + tracing spikes, CI on Windows, frozen `isg-core` traits | Group All ≤ 2 s **and** ≥95% correct count on the corpus. If not → escalate fallbacks before continuing |
| **1 — Shell, Jobs, Persistence** | 3–5 | Tauri shell, T0/T1/T2 job engine with cancellation & preemption, SQLite, streaming import, virtualized library, `.isgproj` atomic save, blake3 cache | 10,000 files, 0 crashes; kill mid-save still opens |
| **2 — Auto Vectorization** | 6–9 | Full pipeline (§3.3), 7 presets, quality scoring, batch orchestration, metrics, A/B comparator UI | C1 1000 imgs ≤ 90 s; SSIM ≥ 0.97; 0 invalid SVGs; RSS ≤ 2 GB |
| **3 — Auto Grouping ★** | 10–13 | RLE CCL, merge, watershed, containment, grid, confidence, mask cache, Group All UI + overlay + Split Here | C2 ≤ 2.0 s **and exactly correct group count**; C4 ≥ 90%; C5 zero false splits; byte-deterministic |
| **4 — SVG Editor** | 14–18 | 4A WASM+canvas+undo · 4B transforms/groups/panels/snap · 4C node editing+booleans+SVG import | 5000 paths @ 60 fps; `undo(do(x))==x` over 10k random sequences |
| **5 — Sheet Generator** | 19–21 | Layout, Auto Leveling, metadata derivation + grid + CSV wizard, all exporters incl. PDF | 1000 icons → `ink_size_cv < 0.05`; opens cleanly in Inkscape/Illustrator/Chrome |
| **6 — Review System** | 22–24 | Quality flags, duplicate cascade, outliers, review workspace, keyboard triage, audit export | 1000 icons triaged ≤ 20 min by 3 users; dupe recall ≥ 0.95 / precision ≥ 0.90 |
| **7 — Hardening & Release** | 25–28 | Perf/memory passes, crash resilience, packaging (offline WebView2 bootstrapper, code signing), offline verification, a11y, docs, beta | USB install with networking disabled works on Windows; zero P0/P1; NPS ≥ 40 |

**Solo-developer note:** 7–9 months realistic; cut T2 node-level path editing (Phase 4C) to post-1.0 if the timeline is threatened — it's ~40% of editor effort for ~10% of the value in this workflow.

**Three scheduling traps:**
1. Segmentation tuning (Phase 3) is the least predictable work — budget 2× and keep test icon sets on hand.
2. Node editing (4C) is timeboxed at 2 weeks or it ships later.
3. **Start Windows code-signing paperwork in Phase 5**, not Phase 7 — it has multi-day lead times.

---

## 9. Risk Register

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Segmentation/grouping accuracy falls short of the ≥95% target on real-world messy sheets | Medium | High | Phase 0 spike gate — do not proceed to Phase 1 if this fails; keep manual override tools (marquee, Split Here) as a permanent safety valve, not an afterthought |
| Rust/WASM boundary gets violated over time, breaking interactivity performance | Medium | Medium | CI check on every commit (`cargo check --target wasm32-unknown-unknown`); code review checklist item |
| Stroke-weight auto-leveling formula doesn't generalize across icon styles | Medium | Medium | Expose sliders/clamps as user-tunable, not hardcoded; validate against a diverse benchmark corpus, not just one style |
| GPL contamination via a transitive dependency | Low | High | `cargo deny` in CI with hard ban, checked on every PR |
| Windows code-signing delays release | Medium | Medium | Start paperwork in Phase 5 (see §8) |
| Solo developer burnout / timeline slip on a 7–9 month project | Medium | High | Ship Phase 0–3 first as a usable "vectorize + group" tool even if editor/review lag; treat 4C as cuttable scope |

---

## 10. Testing & CI Strategy

- **Unit tests** per `isg-core` module (vectorize, group, level, review) using the benchmark corpus fixtures.
- **Golden-file tests**: byte-deterministic SVG output for a fixed input + seed must not change across commits without an explicit "golden update" commit.
- **CI matrix (Windows-first, per user constraint)**: build + `cargo test` + `cargo check --target wasm32-unknown-unknown -p isg-core` + `cargo deny check` on every push.
- **Perf regression gate**: benchmark harness runs the C1–C10 corpus and fails CI if latency exceeds the Phase 8 budget table by >20%.
- **Frontend**: `tsc --noEmit`, component tests for the editor's undo/redo invariant (`undo(do(x)) == x`).

---

## 11. Benchmark Corpus (to build in Phase 0)

At minimum, 10 icon sheets (C1–C10) covering:
- C1: large batch (1000+ simple mono icons, grid-aligned) — throughput test
- C2: 100-icon sheet, grid-aligned — latency test (2s target)
- C3: scattered (non-grid) layout
- C4: icons with holes (rings, letters like O/A/B)
- C5: touching/overlapping icons (fragmentation + over-merge stress test)
- C6: mixed stroke weights (thin outline + solid, for auto-leveling validation)
- C7: JPEG-compressed source (artifact robustness)
- C8: colour icons requiring quantization
- C9: near-duplicate icons (review system dedup test)
- C10: deliberately noisy/low-quality scan

Each entry needs a hand-verified ground-truth icon count and bounding boxes for automated accuracy scoring.
