import { describe, expect, it } from "vitest";
import { grade, iconKey, PRESETS, wipeClips } from "./compareModel";

describe("PRESETS", () => {
  it("lists exactly the seven §3.3-⑤ presets with unique doc names", () => {
    expect(PRESETS).toHaveLength(7);
    const names = PRESETS.map((p) => p.name);
    expect(new Set(names).size).toBe(7);
    // The backend (preset_from_name) accepts exactly these.
    expect(names).toEqual([
      "mono-fast",
      "mono-clean",
      "scan",
      "flat-8",
      "flat-cutout",
      "detailed",
      "pixel-art",
    ]);
  });
});

describe("iconKey", () => {
  it("is stable and distinguishes bbox and preset", () => {
    expect(iconKey([8, 8, 16, 16], "flat-8")).toBe("8,8,16,16@flat-8");
    expect(iconKey([8, 8, 16, 16], "flat-8")).toBe(iconKey([8, 8, 16, 16], "flat-8"));
    expect(iconKey([8, 8, 16, 16], "mono-fast")).not.toBe(iconKey([8, 8, 16, 16], "flat-8"));
    expect(iconKey([9, 8, 16, 16], "flat-8")).not.toBe(iconKey([8, 8, 16, 16], "flat-8"));
  });
});

describe("wipeClips", () => {
  const W = 100;
  const H = 40;

  it("clamps the wipe fraction into [0, 1]", () => {
    expect(wipeClips(-0.5, W, H).a).toEqual([0, 0, 0, H]);
    expect(wipeClips(2, W, H).a).toEqual([0, 0, W, H]);
  });

  it("wipe 0 shows only B, wipe 1 shows only A", () => {
    const zero = wipeClips(0, W, H);
    expect(zero.a).toEqual([0, 0, 0, H]);
    expect(zero.b).toEqual([0, 0, W, H]);
    const one = wipeClips(1, W, H);
    expect(one.a).toEqual([0, 0, W, H]);
    expect(one.b).toEqual([W, 0, 0, H]);
  });

  it("splits at the rounded divider and always covers the canvas exactly once", () => {
    const half = wipeClips(0.5, W, H);
    expect(half.a).toEqual([0, 0, 50, H]);
    expect(half.b).toEqual([50, 0, 50, H]);
    // Odd width: rounding favours A by one pixel, coverage still exact.
    const odd = wipeClips(0.5, 101, H);
    expect(odd.a[2]).toBe(51);
    expect(odd.b[2]).toBe(50);
    // Invariant sweep: no gaps, no overlap, heights preserved.
    for (let i = 0; i <= 20; i++) {
      const c = wipeClips(i / 20, 97, 31);
      expect(c.a[2] + c.b[2]).toBe(97);
      expect(c.a[0]).toBe(0);
      expect(c.b[0]).toBe(c.a[2]);
      expect(c.a[3]).toBe(31);
      expect(c.b[3]).toBe(31);
    }
  });

  it("is monotonically non-decreasing in the divider position", () => {
    let prev = -1;
    for (let i = 0; i <= 100; i++) {
      const x = wipeClips(i / 100, 64, 16).a[2];
      expect(x).toBeGreaterThanOrEqual(prev);
      prev = x;
    }
  });
});

describe("grade", () => {
  it("maps composite bands to verdicts", () => {
    expect(grade(0.97).label).toBe("excellent");
    expect(grade(0.9962).label).toBe("excellent");
    expect(grade(0.9699).label).toBe("good");
    expect(grade(0.92).label).toBe("good");
    expect(grade(0.9199).label).toBe("fair");
    expect(grade(0.85).label).toBe("fair");
    expect(grade(0.8499).label).toBe("poor");
    expect(grade(0).label).toBe("poor");
  });

  it("always returns a tailwind text colour class", () => {
    for (const c of [0, 0.5, 0.9, 0.95, 0.99, 1]) {
      expect(grade(c).className).toMatch(/^text-\w+-400$/);
    }
  });
});
