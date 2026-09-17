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
  },
  build: {
    target: "es2021",
    outDir: "dist",
    emptyOutDir: true,
  },
});
