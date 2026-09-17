import { describe, expect, it } from "vitest";
import { virtualWindow } from "./virtualWindow";

describe("virtualWindow", () => {
  const opts = { rowHeight: 100, columns: 10 };

  it("renders nothing for an empty collection", () => {
    const w = virtualWindow(0, 500, 0, opts);
    expect(w.start).toBe(0);
    expect(w.end).toBe(0);
    expect(w.totalHeight).toBe(0);
  });

  it("covers the first screen at scrollTop 0 with overscan", () => {
    const w = virtualWindow(0, 500, 1000, opts); // 5 visible rows, 4 overscan
    expect(w.start).toBe(0);
    expect(w.end).toBeGreaterThanOrEqual(5 * 10);
    expect(w.end).toBeLessThanOrEqual(1000);
    expect(w.offsetY).toBe(0);
    expect(w.totalHeight).toBe(100 * 100);
  });

  it("slides the window with scroll and clamps at the end", () => {
    const w = virtualWindow(5_000, 500, 1_000, opts); // row 50 first visible
    expect(w.start).toBe((50 - 4) * 10);
    expect(w.end).toBeLessThanOrEqual(1000);
    // Deep past the end: window clamps to the last row.
    const tail = virtualWindow(99_999, 500, 1_000, opts);
    expect(tail.end).toBe(1_000);
    expect(tail.start).toBeLessThan(1_000);
  });

  it("keeps every scrolled-to item inside the rendered slice", () => {
    // Invariant sweep: for any scroll position, the first visible item is
    // within [start, end).
    for (let scroll = 0; scroll <= 9_900; scroll += 137) {
      const w = virtualWindow(scroll, 500, 1_000, opts);
      const firstVisible = Math.floor(scroll / 100) * 10;
      expect(firstVisible).toBeGreaterThanOrEqual(w.start);
      expect(firstVisible).toBeLessThan(Math.max(w.end, 1));
    }
  });

  it("handles a single partial row", () => {
    const w = virtualWindow(0, 500, 7, opts);
    expect(w.start).toBe(0);
    expect(w.end).toBe(7);
    expect(w.totalHeight).toBe(100);
  });
});
