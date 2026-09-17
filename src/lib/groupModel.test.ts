import { describe, expect, it } from "vitest";
import type { Bbox } from "./compareModel";
import {
  boxesIntersect,
  clampSensitivity,
  dragBox,
  groupColor,
  groupLabel,
  groupStatuses,
  isClick,
  marqueeSelection,
  overlaySummary,
  previewScale,
  previewToSheet,
  SENSITIVITY_SLIDERS,
  sliderRequest,
  splitHereSummary,
  unionBbox,
  type GroupingDto,
  type SensitivityDto,
  type SheetPreviewDto,
} from "./groupModel";

function dto(groups: Bbox[], warnings: GroupingDto["warnings"] = []): GroupingDto {
  return {
    sheetId: "ab".repeat(16),
    width: 300,
    height: 300,
    groups: groups.map((bbox) => ({ bbox, area: bbox[2] * bbox[3] })),
    warnings,
    confidence: 0.925,
    reviewGroups: warnings.length,
    statusLine: "Grouped 3 icons in 0.31 s · confidence 93% · 1 group needs review",
    elapsedMs: 310,
    maskCacheHit: true,
    manualEdits: 0,
    sensitivity: { mergeGapFrac: 0.35, mergeAreaRatio: 1.75, noiseMinArea: 16, gridRegularityMin: 0.75 },
    hint: { gridX: false, gridY: false, cellsX: 0, cellsY: 0, valleyX: [], valleyY: [] },
    stats: {
      input: 3,
      output: 3,
      merges: 0,
      restored: 0,
      flagged: 0,
      resplit: 0,
      iterations: 0,
      medianH: 24,
      medianArea: 576,
      elapsedMs: 8,
    },
  };
}

describe("group statuses and colours", () => {
  it("maps the warning index onto a per-group status", () => {
    const d = dto(
      [
        [0, 0, 10, 10],
        [20, 0, 10, 10],
        [40, 0, 10, 10],
      ],
      [
        { group: 1, kind: "spansMultipleCells", label: "spans multiple cells" },
        { group: 2, kind: "restoredFromMerge", label: "restored" },
      ],
    );
    expect(groupStatuses(d)).toEqual(["ok", "spansMultipleCells", "restoredFromMerge"]);
  });

  it("ignores a warning whose index is out of range and keeps the stronger kind", () => {
    const d = dto(
      [
        [0, 0, 10, 10],
        [20, 0, 10, 10],
      ],
      [
        { group: 99, kind: "restoredFromMerge", label: "restored" },
        { group: 0, kind: "restoredFromMerge", label: "restored" },
        { group: 0, kind: "spansMultipleCells", label: "spans" },
      ],
    );
    expect(groupStatuses(d)).toEqual(["spansMultipleCells", "ok"]);
  });

  it("gives adjacent groups different hues and stays in range", () => {
    const hues = [0, 1, 2, 3, 4, 5].map((i) => groupColor(i));
    expect(new Set(hues).size).toBe(6);
    for (const h of hues) expect(h).toMatch(/^hsl\(\d+\.\d 70% 58%\)$/);
    expect(groupColor(0)).toBe("hsl(0.0 70% 58%)");
  });

  it("labels a group with its index and size", () => {
    expect(groupLabel(dto([[4, 5, 26, 19]]), 0)).toBe("0 · 26×19");
    expect(groupLabel(dto([[4, 5, 26, 19]]), 7)).toBe("");
  });
});

describe("marquee geometry", () => {
  const groups = dto([
    [10, 10, 20, 20],
    [40, 10, 20, 20],
    [80, 80, 20, 20],
  ]).groups;

  it("selects only the groups the box overlaps", () => {
    expect(marqueeSelection(groups, [0, 0, 45, 40])).toEqual([0, 1]);
    expect(marqueeSelection(groups, [30, 30, 10, 10])).toEqual([]);
    expect(marqueeSelection(groups, [0, 0, 300, 300])).toEqual([0, 1, 2]);
  });

  it("treats touching edges as no overlap", () => {
    expect(boxesIntersect([0, 0, 10, 10], [10, 0, 10, 10])).toBe(false);
    expect(boxesIntersect([0, 0, 10, 10], [9, 0, 10, 10])).toBe(true);
  });

  it("unions boxes and returns null for none", () => {
    expect(unionBbox([[10, 20, 5, 5], [30, 5, 10, 10]])).toEqual([10, 5, 30, 20]);
    expect(unionBbox([])).toBeNull();
  });

  it("normalizes a drag from any direction and clamps to the sheet", () => {
    expect(dragBox([50, 60], [10, 20], 300, 300)).toEqual([10, 20, 40, 40]);
    expect(dragBox([10, 10], [10, 10], 300, 300)).toEqual([10, 10, 1, 1]);
    expect(dragBox([-20, -5], [400, 400], 300, 200)).toEqual([0, 0, 300, 200]);
  });

  it("distinguishes a click from a drag", () => {
    expect(isClick([100, 100], [102, 101])).toBe(true);
    expect(isClick([100, 100], [140, 100])).toBe(false);
  });
});

describe("sensitivity sliders", () => {
  it("clamps every knob into the native envelope", () => {
    const wild: SensitivityDto = {
      mergeGapFrac: 9,
      mergeAreaRatio: 0,
      noiseMinArea: -5,
      gridRegularityMin: Number.NaN,
    };
    expect(clampSensitivity(wild)).toEqual({
      mergeGapFrac: 1,
      mergeAreaRatio: 1,
      noiseMinArea: 0,
      gridRegularityMin: 0, // NaN falls back to the slider min
    });
  });

  it("keeps a slider request inside the range the backend accepts", () => {
    const base = clampSensitivity({
      mergeGapFrac: 0.35,
      mergeAreaRatio: 1.75,
      noiseMinArea: 16,
      gridRegularityMin: 0.75,
    });
    expect(sliderRequest(base, "mergeAreaRatio", 99).mergeAreaRatio).toBe(8);
    expect(sliderRequest(base, "mergeAreaRatio", -1).mergeAreaRatio).toBe(1);
    expect(sliderRequest(base, "mergeGapFrac", 0.5).mergeGapFrac).toBe(0.5);
    // The slider ranges must match SensitivityParams::RANGES on the native side.
    const ranges = Object.fromEntries(
      SENSITIVITY_SLIDERS.map((s) => [s.key, [s.min, s.max] as [number, number]]),
    );
    expect(ranges.mergeGapFrac).toEqual([0.05, 1]);
    expect(ranges.mergeAreaRatio).toEqual([1, 8]);
    expect(ranges.noiseMinArea).toEqual([0, 512]);
    expect(ranges.gridRegularityMin).toEqual([0, 1]);
  });
});

describe("preview mapping", () => {
  const preview: SheetPreviewDto = {
    png: "AAAA",
    width: 512,
    height: 256,
    sheetWidth: 2048,
    sheetHeight: 1024,
  };

  it("scales sheet pixels into preview pixels and back", () => {
    expect(previewScale(preview)).toBe(0.25);
    expect(previewToSheet(preview, 128, 64)).toEqual([512, 256]);
    expect(previewToSheet(preview, 0, 0)).toEqual([0, 0]);
  });

  it("degrades to 1:1 rather than dividing by zero", () => {
    expect(previewScale({ ...preview, sheetWidth: 0 })).toBe(1);
  });
});

describe("summary lines", () => {
  it("renders the overlay summary and the Split Here result", () => {
    const d = dto([
      [0, 0, 10, 10],
      [20, 0, 10, 10],
      [40, 0, 10, 10],
    ]);
    expect(overlaySummary(d)).toBe("3 icons · 93% confidence · none need review · 310 ms");
    expect(overlaySummary({ ...d, reviewGroups: 2 })).toContain("2 need review");
    expect(
      splitHereSummary({ split: true, regions: 2, groupIndex: 1, elapsedMs: 0.42, report: d }),
    ).toBe("Split Here: 2 regions · 0.42 ms");
    expect(
      splitHereSummary({ split: false, regions: 0, groupIndex: 1, elapsedMs: 0.11, report: d }),
    ).toBe("Split Here: refused (no watershed structure) · 0.11 ms");
  });
});
