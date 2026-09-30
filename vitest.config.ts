import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    // `.tsx` is included because component semantics (labels, roles, live
    // regions) are only checkable by rendering a component, and a rendering
    // test needs JSX. The environment stays `node`: `react-dom/server` renders
    // to static markup, so no DOM implementation is required or wanted.
    include: ["src/**/*.test.{ts,tsx}"],
    environment: "node",
  },
});
