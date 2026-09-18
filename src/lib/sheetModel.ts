/**
 * Sheet generator model (Phase 5, ARCHITECTURE.md §3.5): the DTOs the three
 * sheet commands speak, and the arithmetic the wizard needs *before* it asks
 * the backend — so a slider can show the sheet size it is about to produce
 * without a round trip.
 *
 * Every clamp here mirrors `SheetSpec::from_wire` on the native side, and the
 * layout mirrors `GridLayout::solve`. The mirrored numbers are pinned by tests
 * against the same values the Rust unit tests assert (4 icons over 2 columns =
 * 152 px, an empty sheet = 16 px), because a preview that disagrees with the
 * export is worse than no preview.
 */

/** The cell spec the wizard edits; the native side clamps it. */
export interface SheetSpecDto {
  /** Cell side in pixels. */
  cell: number;
  /** Padding inside each cell. */
  padding: number;
  /** Gap between cells. */
  gap: number;
  /** Margin around the sheet. */
  margin: number;
  /** Cells per row — a maximum: the sheet shrinks to its content. */
  columns: number;
  /** Fraction of the inner box the ink should fill. */
  inkRatio: number;
  /** `center` | `optical` | `baseline`. */
  placement: SheetPlacement;
}

/** §3.5's three anchoring modes. */
export type SheetPlacement = "center" | "optical" | "baseline";

/** What a plan request carries (§3.5 + the metadata wizard). */
export interface SheetPlanRequest {
  sheetId: string;
  spec: SheetSpecDto;
  /** Names the `{sheet}` token; omitted means the source file's stem. */
  sheetStem?: string;
  /** The preset label the metadata and the tags carry. */
  preset: string;
  /** `{sheet}`, `{index}`, `{row}`, `{col}`, `{preset}`, with `:03` padding. */
  namePattern: string;
}

/** The CSV wizard's choices. */
export interface SheetCsvDto {
  delimiter: string;
  header: boolean;
  /** Column headers, in order. Unknown names are dropped by the backend. */
  columns: string[];
}

/** The leveling report, as the summary panel shows it. */
export interface SheetReportDto {
  inkSizeCv: number;
  strokeCv: number;
  baselineSpread: number;
  medianStroke: number;
  medianSolidity: number;
  overflowBackoffs: number;
  strokeClamped: number;
  solidityClamped: number;
}

/** One icon's place on the sheet, plus the metadata derived for it. */
export interface SheetPlacementDto {
  id: number;
  index: number;
  row: number;
  col: number;
  cellX: number;
  cellY: number;
  x: number;
  y: number;
  w: number;
  h: number;
  scale: number;
  flags: string[];
  name: string;
  slug: string;
  file: string;
}

/** The whole plan: the sheet's size, the report, one entry per icon. */
export interface SheetPlanDto {
  width: number;
  height: number;
  columns: number;
  rows: number;
  icons: number;
  cell: number;
  inkRatio: number;
  placement: SheetPlacement;
  report: SheetReportDto;
  placements: SheetPlacementDto[];
}

/** One written file, with the second reader's verdict. */
export interface SheetFileDto {
  format: string;
  path: string;
  bytes: number;
  evidence: string;
}

/** An export's result: the plan it was made from, and every file. */
export interface SheetExportDto {
  plan: SheetPlanDto;
  files: SheetFileDto[];
}

/** The CSV wizard's preview: the table, and the file itself. */
export interface SheetCsvPreviewDto {
  columns: string[];
  rows: string[][];
  text: string;
}

/** What an export writes when the caller does not choose. */
export const DEFAULT_FORMATS = ["svg", "pdf", "png", "csv"] as const;

/** Every format the exporter understands. */
export const ALL_FORMATS = ["svg", "pdf", "png", "csv", "icons"] as const;

/**
 * `SheetSpec::default()`: §3.5's example — 64 px cells, 8 px of padding, gap
 * and margin, sixteen columns, ink at 80 % of the inner box.
 */
export const DEFAULT_SHEET_SPEC: SheetSpecDto = {
  cell: 64,
  padding: 8,
  gap: 8,
  margin: 8,
  columns: 16,
  inkRatio: 0.8,
  placement: "center",
};

/** The placement modes in the order the wizard offers them. */
export const PLACEMENTS: { id: SheetPlacement; label: string; hint: string }[] = [
  {
    id: "center",
    label: "centre",
    hint: "geometrically centred — what most sheets want",
  },
  {
    id: "optical",
    label: "optical",
    hint: "centred, then shifted by half the ink centroid offset",
  },
  {
    id: "baseline",
    label: "baseline",
    hint: "sitting on the inner box's bottom edge",
  },
];

/** The slider envelope, mirroring `SheetSpec::from_wire`'s clamps. */
export const SPEC_RANGES = {
  cell: [8, 4096],
  padding: [0, 2048],
  gap: [0, 4096],
  margin: [0, 4096],
  columns: [1, 4096],
  inkRatio: [0.05, 1],
} as const;

/**
 * The numeric controls the wizard shows, in the order it shows them. `step` is
 * what the slider uses; the value is still clamped by `clampSpec`.
 */
export const SHEET_SLIDERS: {
  key: Exclude<keyof SheetSpecDto, "placement">;
  label: string;
  min: number;
  max: number;
  step: number;
  hint: string;
}[] = [
  { key: "cell", label: "cell", min: 16, max: 256, step: 1, hint: "cell side, px" },
  { key: "padding", label: "padding", min: 0, max: 64, step: 1, hint: "inset inside each cell" },
  { key: "gap", label: "gap", min: 0, max: 64, step: 1, hint: "between cells" },
  { key: "margin", label: "margin", min: 0, max: 256, step: 1, hint: "around the sheet" },
  { key: "columns", label: "columns", min: 1, max: 64, step: 1, hint: "cells per row (a maximum)" },
  { key: "inkRatio", label: "ink ratio", min: 0.2, max: 1, step: 0.01, hint: "ink ÷ inner box" },
];

/** The column vocabulary, mirroring `Column::ALL` in the native CSV writer. */
export const CSV_COLUMNS = [
  "index",
  "name",
  "slug",
  "tags",
  "file",
  "row",
  "col",
  "width",
  "height",
  "ink",
  "stroke",
  "solidity",
  "area",
  "preset",
  "colours",
  "id",
] as const;

/** The columns a fresh export uses, mirroring `CSV_COLUMNS` on the native side. */
export const DEFAULT_CSV_COLUMNS = [
  "index",
  "name",
  "slug",
  "tags",
  "file",
  "row",
  "col",
  "width",
  "height",
] as const;

/** The wizard's defaults: comma-separated, with a header row. */
export const DEFAULT_CSV: SheetCsvDto = {
  delimiter: ",",
  header: true,
  columns: [...DEFAULT_CSV_COLUMNS],
};

/** `{sheet}-{index:03}` — the pattern `CsvOptions::default()` carries. */
export const DEFAULT_NAME_PATTERN = "{sheet}-{index:03}";

function clampInt(raw: number, min: number, max: number, fallback: number): number {
  if (!Number.isFinite(raw)) return fallback;
  return Math.min(max, Math.max(min, Math.round(raw)));
}

/**
 * The spec the backend will actually use.
 *
 * A cell smaller than its padding is the one combination that would make the
 * inner box vanish, so padding is capped at half the cell — the same rule, and
 * the same reason, as `SheetSpec::from_wire`.
 */
export function clampSpec(spec: SheetSpecDto): SheetSpecDto {
  const cell = clampInt(spec.cell, SPEC_RANGES.cell[0], SPEC_RANGES.cell[1], 64);
  return {
    cell,
    padding: Math.min(clampInt(spec.padding, 0, SPEC_RANGES.padding[1], 8), Math.floor(cell / 2)),
    gap: clampInt(spec.gap, 0, SPEC_RANGES.gap[1], 8),
    margin: clampInt(spec.margin, 0, SPEC_RANGES.margin[1], 8),
    columns: clampInt(spec.columns, 1, SPEC_RANGES.columns[1], 16),
    inkRatio: Number.isFinite(spec.inkRatio)
      ? Math.min(1, Math.max(0.05, spec.inkRatio))
      : DEFAULT_SHEET_SPEC.inkRatio,
    placement: PLACEMENTS.some((p) => p.id === spec.placement) ? spec.placement : "center",
  };
}

/** The inner box's side: `cell − 2·padding`, floored at zero. */
export function innerSide(spec: SheetSpecDto): number {
  return Math.max(0, spec.cell - 2 * spec.padding);
}

/** §3.5's target ink: the inner box at the requested ratio. */
export function targetInk(spec: SheetSpecDto): number {
  return innerSide(spec) * spec.inkRatio;
}

/** `2·margin + n·cell + (n−1)·gap`, saturating at 0 for an empty run. */
function span(spec: SheetSpecDto, n: number): number {
  if (n <= 0) return 2 * spec.margin;
  return 2 * spec.margin + n * spec.cell + (n - 1) * spec.gap;
}

/** A resolved grid, mirroring `GridLayout`. */
export interface SheetGridDto {
  columns: number;
  rows: number;
  width: number;
  height: number;
}

/**
 * The grid the native side will solve for `count` icons: `columns` is a
 * maximum and the sheet shrinks to its content, so three icons asked for
 * sixteen columns are one row of three.
 */
export function solveGrid(spec: SheetSpecDto, count: number): SheetGridDto {
  if (count <= 0) return { columns: 0, rows: 0, width: span(spec, 0), height: span(spec, 0) };
  const columns = Math.max(1, Math.min(spec.columns, count));
  const rows = Math.ceil(count / columns);
  return {
    columns,
    rows,
    width: span(spec, columns),
    height: span(spec, rows),
  };
}

/** `{sheet}`, `{index}`, `{row}`, `{col}` and `{preset}`, expanded client-side. */
export function expandPattern(
  pattern: string,
  values: {
    sheet: string;
    index: number;
    row: number;
    col: number;
    preset: string;
  },
): string {
  return pattern.replace(/\{(\w+)(?::(\d+))?\}/g, (whole, token: string, pad?: string) => {
    const raw =
      token === "sheet"
        ? values.sheet
        : token === "preset"
          ? values.preset
          : token === "index"
            ? String(values.index)
            : token === "row"
              ? String(values.row)
              : token === "col"
                ? String(values.col)
                : null;
    if (raw === null) return whole;
    return pad ? raw.padStart(Number(pad), "0") : raw;
  });
}

/** `slugify`'s rule: lowercase ASCII, single dashes, nothing else. */
export function slugify(name: string): string {
  const out = name
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
  return out || "icon";
}

const FLAG_LABELS: Record<string, string> = {
  overflow: "overflow — backed off to a pure fit",
  strokeClamped: "stroke correction clamped",
  solidityClamped: "solidity correction clamped",
};

/** A flag's human label (an unknown flag is shown as-is, never hidden). */
export function flagLabel(flag: string): string {
  return FLAG_LABELS[flag] ?? flag;
}

/** Every flag on a placement, as labels. */
export function placementFlags(placement: SheetPlacementDto): string[] {
  return placement.flags.map(flagLabel);
}

/** `0.0175` — the headline number, at the precision the report uses. */
export function formatCv(value: number): string {
  if (!Number.isFinite(value)) return "—";
  return value.toFixed(4);
}

/**
 * Coefficient of variation (`σ/µ`) — `level::cv`'s definition, mirrored so
 * the browser mock reports the same number the native leveler would.
 */
export function cv(values: number[]): number {
  if (values.length === 0) return 0;
  const mean = values.reduce((a, b) => a + b, 0) / values.length;
  if (!(mean > 0)) return 0;
  const variance = values.reduce((a, b) => a + (b - mean) ** 2, 0) / values.length;
  return Math.sqrt(variance) / mean;
}

/** Pixel counts the way a status line reads them. */
export function formatPx(value: number): string {
  if (!Number.isFinite(value)) return "—";
  return `${value.toFixed(1)} px`;
}

/** One line summarising the leveling, for the panel's status row. */
export function reportLine(report: SheetReportDto, icons: number): string {
  const flags = report.overflowBackoffs + report.strokeClamped + report.solidityClamped;
  return [
    `${icons} icons`,
    `ink-size cv ${formatCv(report.inkSizeCv)}`,
    `stroke cv ${formatCv(report.strokeCv)}`,
    `median stroke ${formatPx(report.medianStroke)}`,
    `baseline ${formatPx(report.baselineSpread)}`,
    flags === 0 ? "no flags" : `${flags} flagged`,
  ].join(" · ");
}

/** `1160×4616 px · 16 columns · 64 rows · 64 px cells`. */
export function planSizeLine(plan: SheetPlanDto): string {
  return (
    `${plan.width}×${plan.height} px · ${plan.columns} columns · ${plan.rows} rows · ` +
    `${plan.cell} px cells · ${plan.placement}`
  );
}

/** A file's size the way the export list shows it. */
export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} kB`;
  return `${(bytes / (1024 * 1024)).toFixed(2)} MB`;
}

/** An export request: everything a plan needs, plus what to write and where. */
export interface SheetExportRequest extends SheetPlanRequest {
  csv: SheetCsvDto;
  /** `svg`, `pdf`, `png`, `csv` and/or `icons`; empty means the first four. */
  formats: string[];
  /** Directory the files are written into. */
  outDir: string;
  /** Pixels per sheet pixel for the PNG. */
  rasterScale: number;
  /** `#rrggbb` paper colour; omitted keeps the sheet transparent. */
  background?: string;
}

/** The name of the sheet files a plan request will produce. */
export function sheetStem(request: SheetPlanRequest): string {
  return request.sheetStem?.trim() || "sheet";
}
