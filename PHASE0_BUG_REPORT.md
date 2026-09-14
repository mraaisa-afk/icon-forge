# Phase 0 Patch Review — Bug Report

**Reviewer:** Arena Agent (partner review, per request "find out if code have bugs")
**Date:** 2026-09-15
**Scope:** `icon-forge_patch.md` (gist `283131fe`, 134,903 bytes, complete) reviewed against `docs/ARCHITECTURE.md` v1.0 and the Phase 0 prompt instructions.
**Verification limit:** this sandbox has no `crates.io` access, so nothing could be compiled here. Everything below is from close reading of the full source. The two failing unit tests (F3, F4) and the generator defect (F1) are hand-traced and high-confidence; items marked ⚠ need one CI run to confirm.

---

## Verdict

The skeleton is genuinely good (keystone respected, sane trait seams, deterministic-corpus design, Windows-first CI, licensing policy present). **But the patch has never been proven to run.** It contains two unit tests that fail against their own implementation, a corpus generator that paints blank sheets, a spike whose grouping algorithm cannot pass the exit gate it defines, and one committed file that will break CI and your Windows build on first contact. **Phase 0 exit criteria are not met by this code as shipped.**

| ID | Severity | Area | One-line summary |
|---|---|---|---|
| F1 | **P0** | `tools/gen-corpus` | `cell != 0` sentinel drops every black (color 0) icon — sheets 03/06/07/09 render blank while truth JSON still counts their icons |
| F2 | **P0** | `spike/groupall` group+gate | "bars" icons are 3 disconnected components; the spike has no fragment merge, so ~9/10 sheets miscount → exit gate cannot pass |
| F3 | **P0** | `spike/groupall/src/group.rs` test | Diagonal-touch test is missing its bridge pixel → test fails (asserts 3 groups, gets 4) |
| F4 | **P0** | `crates/isg-core/src/lib.rs` test | `bbox_expand_clamps_to_canvas` expectations computed for n=5, code calls n=10 → test fails |
| F5 | **P0** | `.cargo/config.toml` | Committed offline source-replacement to `/opt/rust-dl/vendor` breaks `cargo` for everyone without that directory (CI + your Windows box) |
| F6 | **P1** ⚠ | `.cargo-deny.toml` + CI | cargo-deny reads `deny.toml`, not `.cargo-deny.toml` → GPL ban is not actually enforced; `skip-workspace` is likely an invalid key |
| F7 | **P1** ⚠ | CI clippy | `cargo clippy -D warnings` fails on `uf_find`/`uf_union` (`&mut Vec<i32>` → `clippy::ptr_arg`) |
| F8 | **P1** | `group.rs` CCL | Missing `nx >= w` bounds check → pixel at last column wraps and can union with `(0, y)` of the same row (latent, not triggered by corpus) |
| F9 | **P1** | spec conformance | Corpus doesn't cover C1 (1000+ batch), C2 (100-icon latency), C7 JPEG, C8 colour, C9 near-dupes, C10 noisy — the "2 s / 100 icons" keystone claim is untested |
| F10 | **P2** | `isg-core` freeze | Frozen traits use `Vec<bool>` masks + per-pixel i32 CCL, contradicting the bit-packed 1-bpp / RLE-CCL mandate; the freeze locks the slow representation in |
| F11 | **P2** | `trace.rs` | Crop re-binarizes at absolute 128 instead of reusing the mask — mid-tone or light-on-dark icons would trace to empty SVGs (hidden: corpus is dark-on-light only) |
| F12 | **P2** | `isg-core` vs ARCHITECTURE §7 | Frozen surface (ForegroundMasker/GroupingStrategy/VectorTracer/SheetPipeline) ≠ documented sketch (Vectorizer/Grouper/Leveler/ReviewScorer); Leveler & ReviewScorer missing entirely |
| F13 | **P2** | `gen-corpus` | `debug_assert!` gap-invariant is compiled out in release CI; `build_grid` `clamp()` can panic if `margin/2 > w - s - margin/2` |
| F14 | **P3** | docs/quirks | `trace.rs` doc comment inverted ("white-on-black" vs actual black-on-white); "lshape" actually renders a square outline (abs-symmetry); toolchain pin drift (scripts: 1.94.1 vs `rust-toolchain.toml`: stable) |

---

## Detailed findings

### F1 (P0) — Corpus generator never paints black icons

`tools/gen-corpus/src/main.rs`, `Sheet::stamp`:

```rust
let bb = rasterize_shape(shape, s, t, color, &mut cell)?;   // cell[idx] = color
...
if cell[(y * s as i32 + x) as usize] != 0 {                 // ← sentinel bug
    self.px[i] = color;
}
```

`rasterize_shape` marks filled cells by writing `color` into a zeroed cell buffer; `stamp` then paints only cells that are non-zero — i.e., it uses `0` both as "empty" and as a legal ink value.

Consequences:

- Five sheets stamp with **literal `color = 0`** (03_rings_holes, 06_thick_strokes, 07_size_range, 09_near_touching, plus per-icon `rng.range(0, 30)` in 10_edge_corner) → those sheets come out **blank** (background only), while the truth JSON still records 25/16/9/20 icons.
- Sheets with random colors (01, 02, 04, 05, 08) get **phantom icons** whenever the LCG rolls color 0 — with `range(0, 20/24/30/40)` the probability that at least one of 01/02/05/08 is polluted is >99%.
- The README's recovery path ("regenerating the corpus yields identical pixel content") regenerates **blank sheets**, and the committed PNGs (which we cannot inspect through the patch text) can no longer match the generator. Either way the corpus and the generator are mutually inconsistent.

**Fix:** track fill with a separate `Vec<bool>` (or `Option<u8>`) instead of the `0` sentinel; regenerate the corpus; commit fresh PNG + truth. (~10-line change.)

### F2 (P0) — The spike cannot pass its own exit gate (bars fragmentation)

The `bars` shape is three **disconnected** horizontal bars:

```rust
"bars" => {
    let bandc = s as f32 * 0.30;
    let half_t = t as f32 * 0.5;
    dx <= s as f32 * 0.44 && ((dy - bandc).abs() <= half_t || dy <= half_t)
}
```

(the `dy`/`dx` are absolute distances from center, so this yields a middle bar at `|dy| ≤ t/2` and a symmetric pair at `|dy ∓ 0.30 s| ≤ t/2`; stroke `t ≤ s/7 ≈ 0.14 s < 0.30 s` guarantees the gaps). The ground truth counts a `bars` icon as **one** icon (`expected_groups`).

The spike's `CclGrouper` is pure CCL + a `min_area` filter — fragment merging is a Phase 3 feature (§3.4 F1) and is absent here. So every `bars` icon produces **3 groups instead of 1**. From the committed truth JSONs, 8 of 10 sheets contain bars icons (01: 5, 02: 8, 04: 6, 05: 3+, 08: 2, 09: 6, …), so `count_correct` fails on nearly every sheet. Best case ≈ 1/10 exact sheets (10%) versus a requirement of `min_correct_sheets(10) = 10` — **the gate fails by construction**, independent of F1.

Combined with F1 (blank sheets → 0 groups found on 03/06/07/09), `cargo test` cannot go green.

**Fix options (pick one, or a combination):**
1. Keep the gate honest: make the Phase 0 corpus contain only connected shapes (drop `bars` or connect its bars with a stem) — my recommendation, since the Phase 0 spike is explicitly a *segmentation + tracing* spike, not the Phase 3 merge engine; or
2. Add a minimal gap-merge rule to the spike (merge components whose bboxes are within `0.5 × median_h` and have compatible heights — a 30-line foreshadowing of Phase 3); or
3. Regenerate truth as component counts (weakens the gate's meaning; not recommended).

### F3 (P0) — Failing unit test: missing diagonal bridge pixel

`spike/groupall/src/group.rs`, `tests::groups_disconnected_blocks_and_filters_noise`:

```rust
// diagonal touch: two 4x4 blocks connected only diagonally at (14,14)/(15,15)
for y in 10..14 { for x in 10..14 { mask[y * 32 + x] = true; } }   // (10..13)×(10..13)
for y in 15..19 { for x in 15..19 { mask[y * 32 + x] = true; } }   // (15..18)×(15..18)
```

The comment says the blocks touch diagonally *at (14,14)*, but **(14,14) is never set**. Chebyshev distance between (13,13) and (15,15) is 2 — they are *not* 8-adjacent, so the CCL correctly reports them as two components. The assertions then fail:

```rust
assert_eq!(groups.len(), 3, ...);                          // actual: 4
assert_eq!(groups[1].bbox, Bbox::new(10, 10, 9, 9)...);    // actual: (10,10,4,4)
```

**Fix (one line):** `mask[14 * 32 + 14] = true;` — with the bridge pixel the blocks merge and every assertion holds (bbox (10,10)–(18,18) = 9×9 ✓).

### F4 (P0) — Failing unit test: `expand` expectations are for the wrong n

`crates/isg-core/src/lib.rs`, `tests::bbox_expand_clamps_to_canvas`:

```rust
let far = Bbox::new(90, 95, 5, 5).unwrap();
let f = far.expand(10, &canvas);
assert_eq!(f, Bbox::new(85, 90, 15, 10).unwrap());
```

Trace `expand(10, canvas(0,0,100,100))`: `x = max(90−10, 0) = 80`, `y = max(95−10, 0) = 85`, `x2 = min(105, 100) = 100`, `y2 = min(110, 100) = 100` → **(80, 85, 20, 15)**. The expected tuple (85, 90, 15, 10) is exactly `expand(5)` — the test data was written for `n = 5` and the call says `10`. The first assertion of the same test (a=(0,0,10,10), n=5 → (0,0,15,15)) is consistent and passes; only this one fails.

**Fix:** change the call to `far.expand(5, &canvas)` (keeps the intended near-corner clamp scenario) or update the expectation to `(80, 85, 20, 15)`.

### F5 (P0) — Committed `.cargo/config.toml` breaks every non-sandbox build

The patch adds a **tracked** `.cargo/config.toml`:

```toml
[source.crates-io]
replace-with = "vendored-local"
[source.vendored-local]
directory = "/opt/rust-dl/vendor"
```

Its own header says "(gitignored)" — but `.gitignore` ignores `.cargo/config.local.toml`, and the workspace `Cargo.toml` comment references a third name, `.cargo/config.vendor.toml`. Three names, one reality: the file **is committed**. On GitHub Actions (windows leg, first step `cargo check --workspace --release`) and on your Windows machine, cargo will rewrite the crates.io source to `/opt/rust-dl/vendor` — a directory that doesn't exist outside the original agent sandbox — and **every cargo command fails** before a single line compiles.

**Fix:** delete `.cargo/config.toml` from the repo; keep the offline recipe as an untracked template (e.g. `.cargo/config.vendor.toml.example`, ignored by `.gitignore`). This also removes the confusing three-way naming drift.

### F6 (P1, needs CI run ⚠) — License gate is wired to the wrong filename

CI uses `EmbarkStudios/cargo-deny-action@v2`, which reads **`deny.toml`** (or an explicitly passed config). The policy lives in `.cargo-deny.toml` — a filename cargo-deny does not look for — so the GPL/AGPL hard ban (a hard constraint of this project) is silently not enforced. Additionally `[bans] skip-workspace = true` does not appear to be a valid cargo-deny key (I can't verify offline); unknown keys typically make the config parse fail.

**Fix:** rename to `deny.toml`, drop/replace `skip-workspace`, keep `copyleft = "deny"` + the explicit GPL/AGPL `deny` list, and add a tiny CI sanity assertion (e.g. `cargo deny check licenses` failing on a synthetic GPL crate is overkill — at minimum, let the action's own output confirm the policy file was loaded).

### F7 (P1, needs CI run ⚠) — clippy `-D warnings` will fail

`fn uf_find(parent: &mut Vec<i32>, ...)` / `fn uf_union(parent: &mut Vec<i32>, ...)` trigger `clippy::ptr_arg` (`&mut Vec<i32>` → `&mut [i32]`; only indexing is used). With `cargo clippy -- -D warnings` in CI, that's a hard error. There may be more lints — this is just the certain one.

**Fix:** take `&mut [i32]`.

### F8 (P1) — CCL out-of-bounds neighbor check (latent wrap bug)

`spike/groupall/src/group.rs`, pass 1:

```rust
for &(dx, dy) in &[(1, 1), (0, 1), (-1, 1), (1, 0)] {
    let nx = x - dx;
    let ny = y - dy;
    if nx < 0 || ny < 0 { continue; }        // ← no upper bound on nx!
    let ni = (ny * w + nx) as usize;
```

For `dx = -1` (neighbor `(x+1, y-1)`), when `x = w − 1`, `nx = w` and `ni = y*w + 0` — that's pixel **(0, y) of the same row**. A foreground pixel at the last column gets unioned with a foreground pixel at column 0 of its own row. The corpus never triggers it (icons stay ≥2 px from borders, none reach column w−1), so tests stay green — but 10_edge_corner-style border-touching content, full-width rule lines, or any Phase 3 real-world sheet will silently merge opposite-edge icons. This is exactly the class of bug the byte-deterministic corpus was supposed to catch; the corpus just misses this case.

**Fix:** `if nx < 0 || nx >= w || ny < 0 { continue; }`. Add a regression test: a 1-px dot at `(w-1, y)` and another at `(0, y)` must remain two groups.

### F9 (P1) — Corpus coverage vs. the approved spec (§11)

The roadmap's Phase 0 corpus (C1–C10) exists as 10 sheets, but coverage is:

| Spec sheet | Present? |
|---|---|
| C1 large batch 1000+ icons (throughput) | ✗ (max sheet is 64 icons; 253 total) |
| C2 100-icon latency sheet (the "2 s" claim) | ✗ |
| C3 scattered | ✓ (04) |
| C4 holes | ✓ (03, partially 06) |
| C5 touching/overlapping | ~ (09 near-touching, gaps ≥4 px; no true overlaps) |
| C6 mixed stroke weights | ~ (07 size range; stroke variety incidental) |
| C7 JPEG artifacts | ✗ (all lossless PNG) |
| C8 colour / quantization | ✗ (grayscale only) |
| C9 near-duplicates | ✗ |
| C10 noisy scan | ✗ |

Phase 0's stated purpose is to "prove or disprove the 2 s / 100 icons claim" — this corpus proves nothing about 100-icon 4096² sheets, and nothing about JPEG/colour/noise robustness. The implemented gate is internally consistent but much weaker than the approved exit criteria.

**Fix:** add at minimum: a 4096²/100-icon sheet (C2), a 1024-icon sheet (C1), a JPEG-encoded variant of one sheet (C7). Colour (C8) can slip to Phase 2 honestly — but say so in the exit report rather than silently. Note: a 4096² sheet with the current per-pixel `Vec<i32>` CCL allocates ~250 MB + stats vectors and will be slow — which is precisely the signal Phase 0 should surface (see F10).

### F10 (P2) — The freeze locks in a representation the architecture forbids

`ForegroundMasker::foreground -> Vec<bool>` (1 byte/px) and a per-pixel `Vec<i32>` union-find contradict the keystone performance design: "bit-packed 1-bpp masks (no full RGBA buffers)" and "CCL on RLE runs". At 4096² that's ~16 MB mask + ~1 GB label/stats structures versus the architecture's 50–200 KB RLE target, and the 10 ms CCL budget becomes hundreds of ms.

For a throwaway spike this is fine — but this code **freezes** `Vec<bool>` into the Phase 0 API ("breaking changes require explicit re-approval"), guaranteeing a freeze-break in Phase 2/3.

**Fix (decide now, before the freeze means anything):** either (a) freeze `RleMask`/bit-packed types now (they're small: run-length struct + bitset), or (b) mark the current trait surface as spike-scoped and keep the production freeze for the §7 traits. Don't freeze the wrong thing by accident.

### F11 (P2) — Tracer re-binarizes with a hardcoded threshold

`VtracerTracer` builds crops with `v < 128 → black` regardless of the masker's decision (bg-median ± 32). Two failure modes hidden by the all-dark-on-light corpus: an icon pixel with luma in `[bg+32, 128)` is foreground for the grouper but white in the crop (silently dropped from the SVG); a light-on-dark sheet (bg ≈ 30, icons ≈ 200) groups fine and then traces to **empty SVGs** (every crop pixel ≥ 128 → white).

**Fix (Phase 2 seam, cheap to do now):** pass the precomputed mask run-ranges into the crop, or derive the crop threshold from the same `bg` estimate. Also fix the inverted doc comment ("white-on-black" → "black-on-white").

### F12 (P2) — Frozen trait surface diverges from ARCHITECTURE §7

The architecture's frozen-at-Phase-0 sketch defines `Vectorizer`, `Grouper`, `Leveler`, `ReviewScorer`. The patch freezes `ForegroundMasker`, `GroupingStrategy`, `VectorTracer`, `SheetPipeline` — a finer-grained and defensible decomposition, but `Leveler` and `ReviewScorer` (Phases 5–6) are nowhere, and the doc no longer matches the code. Since the freeze is the deliverable of Phase 0, the doc/code mismatch is part of the deliverable.

**Fix:** update §7 to the actual surface (my recommendation — the finer seams are better), and note explicitly that `Leveler`/`ReviewScorer` freeze in their phases.

### F13 (P2) — Generator asserts & panics

- `build_scattered`'s gap invariant is `debug_assert!` — CI runs `cargo test --release`, so it's compiled out exactly where you want it enforced. Use a hard `assert!` (cost is negligible).
- `build_grid`: `tx.clamp(margin / 2, sheet.w - s - margin / 2)` panics when `min > max` (large `s`, small sheet) — latent, but clamp-panic with a grid loop is a nasty way to die. Guard or use `min/max` explicitly.

### F14 (P3) — Small stuff

- `trace.rs` struct doc: "white-on-black" — code produces black-on-white.
- `lshape` renders a square outline (both clauses use `|·|` distances, so it's symmetric) — harmless for counts, but the corpus loses its intended concave-shape case (relevant to Phase 3 watershed).
- `scripts/fetch-toolchain.sh` pins Rust 1.94.1 while `rust-toolchain.toml` says `stable` — fine for its throwaway purpose, but note the drift in the script header.
- `border_median` underflows (`h - 1`) on a zero-height raster — unreachable via `image::open`, noted for completeness.

---

## What the patch gets right (worth keeping)

- **Keystone discipline:** `isg-core` is dependency-free, `#![deny(unsafe_code)]`, no fs/threads; wasm32 check is in CI on both legs. ✔
- **Per-task vtracer construction** in the rayon loop (the pipeline isn't `Send`) — correct and documented. ✔
- **Honest timing:** the gate measures decode + mask + group + trace, not just the easy part. ✔
- **Deterministic corpus design** (LCG, no transcendentals, no anti-aliasing) — byte-identical regeneration is achievable once F1 is fixed. ✔
- **Freeze hygiene:** prelude module, ordinal stability test, documented re-approval rule. ✔
- **CI shape** (windows-primary + trivial ubuntu leg) matches your constraints, and the `push: branches: ["arena/**"]` filter matches this session's branch.

## Recommended fix order

1. F5 (unblocks any local/CI build at all)
2. F3 + F4 (unit tests green)
3. F1 (+ regenerate corpus) and F2 (decide gate philosophy: connected-shapes corpus **or** minimal merge)
4. F8 (+ regression test), F7, F6
5. F9 corpus additions (at least C2 100-icon/4096² and C1 1024-icon) — then re-measure the ≤2 s criterion on real target-scale input
6. F10/F12 freeze decisions *before* tagging Phase 0 complete

## What I need from you

1. **Decision on F2**: keep the Phase 0 corpus to connected shapes (my recommendation), or add a minimal merge to the spike?
2. **Decision on F10**: freeze RLE/bit-packed types now, or mark current traits spike-scoped?
3. Confirmation that the **CI-as-Rust-gate** loop (the patch's own `.github/workflows/ci.yml`, which is also what I proposed earlier) is approved — crates.io remains unreachable from this sandbox, so compile/test truth comes from the Actions run on push; I'll paste results into the exit report.

*— End of report. No code was changed; `docs/ARCHITECTURE.md` was added to the repo as the recovered architecture baseline.*
