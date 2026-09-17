/**
 * Locates the built editor module for tests.
 *
 * The artifact is a build product, so the engine tests skip when it is absent —
 * loudly, and fatally when `ISG_REQUIRE_WASM=1` is set (CI sets it on the step
 * that builds the artifact, so an "accidentally skipped" integration suite fails
 * the run instead of passing quietly).
 */

import { existsSync, readFileSync } from "node:fs";

/** Where a local build leaves the module (override with `ISG_WASM_PATH`). */
export const WASM_PATH =
  process.env.ISG_WASM_PATH ?? "crates/isg-wasm/pkg/isg_wasm.wasm";

/** How to rebuild it — repeated in the skip warning and the failure message. */
export const BUILD_HINT =
  "cargo build --release --target wasm32-unknown-unknown -p isg-wasm";

/** Module bytes as a plain, byte-addressable buffer (never a Node Buffer view). */
export type ArtifactBytes = Uint8Array<ArrayBuffer>;

/** The module bytes, or null when the artifact has not been built yet. */
export function loadArtifact(): ArtifactBytes | null {
  if (!existsSync(WASM_PATH)) {
    if (process.env.ISG_REQUIRE_WASM === "1") {
      throw new Error(`the editor artifact is required but missing: ${WASM_PATH} (build: ${BUILD_HINT})`);
    }
    console.warn(`[editor tests] skipping the module tests: ${WASM_PATH} not found (build: ${BUILD_HINT})`);
    return null;
  }
  // Copied out of Node's pooled Buffer: WebAssembly.instantiate wants a view
  // over a plain ArrayBuffer, and a pooled Buffer may be a view into a larger
  // (even shared) allocation.
  return new Uint8Array(readFileSync(WASM_PATH));
}

/** True when the module tests will run. */
export const artifactAvailable = existsSync(WASM_PATH);
