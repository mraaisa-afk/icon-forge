/**
 * The wizard's arithmetic, pinned against the numbers the native side asserts.
 *
 * These are the Rust unit tests' own values (`SheetSpec::from_wire`'s clamps,
 * `GridLayout::solve`'s 4-icons/2-columns = 152 px, an empty sheet at 16 px),
 * re-stated on the TypeScript side: the preview a designer sees before pressing
 * Export has to be the sheet the exporter writes.
 */

import { describe, expect, it } from "vitest";

import {
  clampSpec,
  DEFAULT_CSV_COLUMNS,
  DEFAULT_SHEET_SPEC,
  expandPattern,
  flagLabel,
  formatBytes,
  formatCv,
  innerSide,
  planSizeLine,
  placementFlags,
  reportLine,
  slugify,
  solveGrid,
  targetInk,
  type SheetPlanDto,
} from "./sheetModel";

describe("clampSpec", () => {
  it("is identity for the documented default", () => {
    expect(clampSpec(DEFAULT_SHEET_SPEC)).toEqual(DEFAULT_SHEET_SPEC);
  });

  it("caps padding at half the cell, so the inner box can never vanish", () => {
    const spec = clampSpec({ ...DEFAULT_SHEET_SPEC, cell: 64, padding: 500 });
    expect(spec.padding).toBe(32);
    expect(innerSide(spec)).toBe(0);
  });

  it("clamps the sliders and falls back on nonsense", () => {
    const spec = clampSpec({
      cell: 4,
      padding: 1,
      gap: -5,
      margin: Number.NaN,
      columns: 0,
      inkRatio: 5,
      placement: "sideways" as never,
    });
    expect(spec.cell).toBe(8);
    expect(spec.gap).toBe(0);
    expect(spec.margin).toBe(8);
    expect(spec.columns).toBe(1);
    expect(spec.inkRatio).toBe(1);
    expect(spec.placement).toBe("center");
  });

  it("falls back to 0.80 for a non-finite ink ratio", () => {
    expect(clampSpec({ ...DEFAULT_SHEET_SPEC, inkRatio: Number.NaN }).inkRatio).toBe(0.8);
    expect(clampSpec({ ...DEFAULT_SHEET_SPEC, inkRatio: 0 }).inkRatio).toBe(0.05);
  });
});

describe("§3.5's target ink", () => {
  it("is (64 − 2·8) · 0.80 = 38.4 px for the default spec", () => {
    expect(innerSide(DEFAULT_SHEET_SPEC)).toBe(48);
    expect(targetInk(DEFAULT_SHEET_SPEC)).toBeCloseTo(38.4, 5);
  });
});

describe("solveGrid", () => {
  it("is 152 px for four icons over two columns", () => {
    const spec = clampSpec({ ...DEFAULT_SHEET_SPEC, columns: 2 });
    expect(solveGrid(spec, 4)).toEqual({
      columns: 2,
      rows: 2,
      width: 152,
      height: 152,
    });
  });

  it("leaves an empty sheet at twice the margin", () => {
    expect(solveGrid(DEFAULT_SHEET_SPEC, 0)).toEqual({
      columns: 0,
      rows: 0,
      width: 16,
      height: 16,
    });
  });

  it("shrinks to its content: three icons asked for sixteen columns are one row of three", () => {
    const grid = solveGrid(DEFAULT_SHEET_SPEC, 3);
    expect(grid).toEqual({ columns: 3, rows: 1, width: 224, height: 80 });
  });

  it("matches the 1024-icon sheet the Phase-5 dry run measured", () => {
    // 2·8 + 16·64 + 15·8 = 1160 wide; 2·8 + 64·64 + 63·8 = 4616 tall.
    expect(solveGrid(DEFAULT_SHEET_SPEC, 1024)).toEqual({
      columns: 16,
      rows: 64,
      width: 1160,
      height: 4616,
    });
  });
});

describe("name derivation", () => {
  it("expands the tokens, with zero padding", () => {
    const values = {
      sheet: "11_c1_batch_grid",
      index: 7,
      row: 1,
      col: 7,
      preset: "flat-8",
    };
    expect(expandPattern("{sheet}-{index:03}", values)).toBe("11_c1_batch_grid-007");
    expect(expandPattern("{preset}/{row}x{col}", values)).toBe("flat-8/1x7");
  });

  it("leaves an unknown token as written rather than dropping it", () => {
    const values = { sheet: "s", index: 1, row: 1, col: 1, preset: "p" };
    expect(expandPattern("{sheet}-{nope}", values)).toBe("s-{nope}");
  });

  it("slugs the way the manifest does", () => {
    expect(slugify("11_c1_batch_grid-007")).toBe("11-c1-batch-grid-007");
    expect(slugify("Flat 8/Cutout")).toBe("flat-8-cutout");
    expect(slugify("***")).toBe("icon");
  });
});

describe("the summary the panel prints", () => {
  it("labels every documented flag and passes an unknown one through", () => {
    expect(flagLabel("overflow")).toContain("overflow");
    expect(flagLabel("somethingNew")).toBe("somethingNew");
    expect(
      placementFlags({
        id: 1,
        index: 1,
        row: 1,
        col: 1,
        cellX: 8,
        cellY: 8,
        x: 0,
        y: 0,
        w: 10,
        h: 10,
        scale: 1,
        flags: ["overflow", "strokeClamped"],
        name: "n",
        slug: "n",
        file: "svg/n.svg",
      }),
    ).toHaveLength(2);
  });

  it("reads the headline number at four decimals", () => {
    expect(formatCv(0.017478)).toBe("0.0175");
    expect(formatCv(Number.NaN)).toBe("—");
  });

  it("summarises the leveling and the sheet size", () => {
    const report = {
      inkSizeCv: 0.0175,
      strokeCv: 0.0645,
      baselineSpread: 4.677,
      medianStroke: 38.76,
      medianSolidity: 0.985,
      overflowBackoffs: 0,
      strokeClamped: 14,
      solidityClamped: 0,
    };
    expect(reportLine(report, 1024)).toBe(
      "1024 icons · ink-size cv 0.0175 · stroke cv 0.0645 · median stroke 38.8 px · " +
        "baseline 4.7 px · 14 flagged",
    );
    expect(reportLine({ ...report, strokeClamped: 0 }, 4)).toContain("no flags");
    const plan: SheetPlanDto = {
      width: 1160,
      height: 4616,
      columns: 16,
      rows: 64,
      icons: 1024,
      cell: 64,
      inkRatio: 0.8,
      placement: "center",
      report,
      placements: [],
    };
    expect(planSizeLine(plan)).toBe("1160×4616 px · 16 columns · 64 rows · 64 px cells · center");
  });

  it("has nine default columns and sixteen in the vocabulary", () => {
    expect(DEFAULT_CSV_COLUMNS).toHaveLength(9);
    expect(DEFAULT_CSV_COLUMNS[0]).toBe("index");
  });

  it("formats file sizes", () => {
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(2048)).toBe("2.0 kB");
    expect(formatBytes(3 * 1024 * 1024)).toBe("3.00 MB");
  });
});
