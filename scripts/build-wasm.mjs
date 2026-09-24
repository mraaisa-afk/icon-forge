#!/usr/bin/env node
/**
 * Builds the editor module and puts it where the webview loads it from.
 *
 * The window fetches `isg_wasm.wasm` relative to the document
 * (`EDITOR_MODULE_URL` in `src/state/editorStore.ts`), i.e. from the built
 * frontend directory, and Vite copies `public/` verbatim into `dist/`. So a
 * production build needs the artifact at `public/isg_wasm.wasm` — without this
 * step the packaged app installs and opens with no editor at all, which is a
 * failure that only shows up in a browser console in a bundled build.
 *
 * `.gitignore` already excludes the destination: it is a build product, and
 * committing it would let the binary drift from the Rust source it came from.
 *
 * Run it before `npm run build` (the release workflow does; `npm run dev`
 * needs it once for the browser-side editor to load):
 *
 *     npm run wasm
 */
import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync, statSync } from "node:fs";
import { dirname } from "node:path";

/** Where cargo leaves the module. */
const ARTIFACT = "target/wasm32-unknown-unknown/release/isg_wasm.wasm";
/** Where the webview looks for it. */
const DEST = "public/isg_wasm.wasm";

const build = spawnSync(
  "cargo",
  ["build", "--release", "--target", "wasm32-unknown-unknown", "-p", "isg-wasm"],
  // Windows resolves `cargo.cmd` only through a shell.
  { stdio: "inherit", shell: process.platform === "win32" },
);
if (build.error) {
  console.error(`could not run cargo: ${build.error.message}`);
  process.exit(1);
}
if (build.status !== 0) {
  process.exit(build.status ?? 1);
}

mkdirSync(dirname(DEST), { recursive: true });
copyFileSync(ARTIFACT, DEST);
const { size } = statSync(DEST);
if (size === 0) {
  console.error(`${DEST} is empty — the editor module did not build`);
  process.exit(1);
}
console.log(`editor module: ${ARTIFACT} -> ${DEST} (${size} bytes)`);
