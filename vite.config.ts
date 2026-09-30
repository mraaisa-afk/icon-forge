import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// The Tauri dev server uses port 5173; in plain-browser dev (no shell) the
// tauri bridge falls back to an in-memory mock so the UI still runs.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    // When the UI is previewed from a remote workspace it is proxied under a
    // per-sandbox host (`…e2b.app`). Vite 5 answers an unknown Host header with
    // 403, so this domain is allowed explicitly rather than switching the check
    // off: `npm run dev` on a developer's own machine still rejects anything
    // that is not localhost.
    allowedHosts: [".e2b.app"],
  },
  build: {
    target: "es2021",
    outDir: "dist",
    emptyOutDir: true,
  },
});
