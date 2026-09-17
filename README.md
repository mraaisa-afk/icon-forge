# icon-forge

Offline, USB-distributable desktop app for the **1000+ icon pipeline**:
vectorize raster icon sheets to SVG, auto-group, auto-level, export.
Tauri 2 + Rust + React/TypeScript. Windows desktop first.

Architecture: [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) (source of truth
for all phases).

## Phase 0 (current)

Foundations + spike. Deliverables in this tree:

| Path | What it is |
|---|---|
| `crates/isg-core` | **Provisional** (spike-scoped, not frozen) wasm-compatible core types & pipeline traits — compiled twice: native + wasm32. CI enforces `cargo check --target wasm32-unknown-unknown -p isg-core`. The freeze is deliberately deferred until the mask/run types are reworked to bit-packed / RLE representation (per ARCHITECTURE §3.4). |
| `spike/groupall` | Throwaway Group All spike: border-median masking → 8-connected CCL → vtracer per-group SVG (rayon). Contains the **exit-criteria gate** (`tests/corpus.rs`). |
| `tools/gen-corpus` | Deterministic generator for the benchmark corpus (fixed-seed LCG, **integer-only rasterization** — bit-exact across the JS/Rust mirrors, connected shapes only). |
| `bench/corpus` | 16 committed sheets + per-sheet ground truth + `manifest.json` (see corpus table below). |
| `scripts/gen-corpus.mjs` + `scripts/verify-corpus.mjs` | JS mirror of the generator (used to author the committed corpus from network-isolated sandboxes) and an independent verifier (re-counts groups from the committed binaries). |
| `deny.toml` | License policy: narrow allow-list, copyleft denied, GPL/AGPL hard-banned (cargo-deny reads this filename by default). |
| `.cargo/config.vendor.toml.example` | Offline-recipe template — copy to `.cargo/config.toml` (gitignored) **only** in network-isolated sandboxes after `python3 scripts/fetch_vendor.py`. Never committed; CI always uses plain crates.io. |
| `.github/workflows/ci.yml` | Windows-first CI: build + tests + benchmark + corpus consistency + wasm gate + fmt/clippy + cargo-deny; trivial ubuntu quick leg. |

### Benchmark corpus (v2, 16 sheets)

| Sheet | C-ref | What it stresses | Icons |
|---|---|---|---|
| `01_basic_grid` | C2' | neat 4×4 grid | 16 |
| `02_mixed_grid` | — | mixed shapes/sizes/colours, 6×6 | 36 |
| `03_rings_holes` | C4 | rings/frames — containment & holes | 25 |
| `04_scattered` | C3 | scattered non-grid layout | 30 |
| `05_dense_grid` | — | dense 8×8 grid | 64 |
| `06_thick_strokes` | C6' | heavy strokes | 16 |
| `07_size_range` | C6 | icon sizes 20→116 px | 9 |
| `08_grid_drift` | — | grid with ±10 px drift | 25 |
| `09_near_touching` | C5' | 4–6 px gaps (no merge stage yet) | 20 |
| `10_edge_corner` | — | icons 2 px off borders/corners | 12 |
| `11_c1_batch_grid` | C1 | **1024-icon 4096² batch** — throughput | 1024 |
| `12_c2_latency_grid` | C2 | **100-icon 4096² grid — the keystone latency sheet** | 100 |
| `13_c7_jpeg_grid` | C7 | JPEG q85 source (artifact robustness) | 25 |
| `14_c8_colour_icons` | C8 | solid-colour icons (RGB) | 16 |
| `15_c9_duplicates` | C9 | near-duplicate icons (Phase 6 dedup seeding) | 16 |
| `16_c10_noisy_scan` | C10 | noisy scan + isolated specks | 20 |

Not yet covered (honest gaps, deferred with the phases that need them):
C5 *overlapping* icons (watershed splitting is Phase 3), true colour
quantization stress (the Phase 2 quantizer), large-scale near-duplicate
variance (Phase 6). The corpus generator makes extending it cheap.

### Phase 0 exit criteria (enforced by `cargo test` and the CLI exit code)

1. **count** — ≥ 95 % of sheets with an exactly correct group count. With 16
   sheets the ceiling makes that **16/16 exact** (stronger than the roadmap
   floor — deliberate).
2. **keystone time** — the C2 sheet (4096², 100 icons) completes Group All in
   **≤ 2000 ms**. This is the roadmap's "2 s / 100 icons" claim, verbatim.
   (The earlier draft's "sum over the whole corpus ≤ 2 s" reading cannot
   survive adding the C1 batch sheet by definition; the 2 s criterion is per
   100-icon sheet per ARCHITECTURE §3.2/§8.)
3. **corpus guard** — whole-corpus total ≤ 5000 ms (CI-friendly regression
   bound, reported but secondary).
4. **zero failed traces** on every sheet.

```sh
# regenerate the corpus (deterministic: identical pixels + truth JSONs;
# PNG/JPEG container bytes may differ by encoder)
cargo run -p isg-gen-corpus -- --out bench/corpus

# run the spike + gate
cargo run -p isg-spike-groupall -- --corpus bench/corpus
cargo test -p isg-spike-groupall --test corpus
```

The committed corpus was authored by `scripts/gen-corpus.mjs`, a bit-exact
mirror of the Rust generator (integer-only rasterization + fixed RNG order).
CI regenerates with the Rust tool and diffs every truth JSON byte-for-byte.

## Roadmap (phase gates enforced; each phase starts only on explicit
confirmation after the previous exit criteria are met)

0. Foundations & spike ← **current**
1. Shell, job system, SQLite (WAL)
2. 8-stage vectorization pipeline (ΔE masking, morphology, quantize, simplify)
3. Auto grouping (RLE CCL, merge, watershed, grid hints, confidence)
4. WASM SVG editor (raw Canvas2D + Path2D)
5. Sheet generator + auto leveling
6. Review system
7. Windows hardening & release (USB-distributable)

## Licensing

No GPL/AGPL anywhere (never potrace/autotrace). `resvg` (MPL-2.0) is used
unmodified behind an adapter. CI runs cargo-deny (`deny.toml`) with a hard
copyleft ban on every push.
