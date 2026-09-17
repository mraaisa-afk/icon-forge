/**
 * Group All model (W12): the DTO shapes the backend returns, plus every piece
 * of overlay geometry as a pure function so it can be unit-tested without a
 * canvas. Colours and labels are view concerns and live here too, next to the
 * status they annotate.
 */

import type { Bbox } from "./compareModel";

/** One group in the overlay, canonical scan order. */
export interface GroupDto {
  bbox: Bbox;
  area: number;
}

/** Why a group needs review. */
export type WarningKind = "spansMultipleCells" | "restoredFromMerge";

export interface WarningDto {
  /** Index into `GroupingDto.groups`. */
  group: number;
  kind: WarningKind;
  /** Short human label. */
  label: string;
}

export interface GridHintDto {
  gridX: boolean;
  gridY: boolean;
  cellsX: number;
  cellsY: number;
  valleyX: number[];
  valleyY: number[];
}

export interface GroupStatsDto {
  input: number;
  output: number;
  merges: number;
  restored: number;
  flagged: number;
  resplit: number;
  iterations: number;
  medianH: number;
  medianArea: number;
  elapsedMs: number;
}

/** The four sensitivity sliders, mirroring the native envelope. */
export interface SensitivityDto {
  mergeGapFrac: number;
  mergeAreaRatio: number;
  noiseMinArea: number;
  gridRegularityMin: number;
}

/** One grouping result — the whole overlay payload. */
export interface GroupingDto {
  sheetId: string;
  width: number;
  height: number;
  groups: GroupDto[];
  warnings: WarningDto[];
  confidence: number;
  reviewGroups: number;
  statusLine: string;
  elapsedMs: number;
  maskCacheHit: boolean;
  manualEdits: number;
  sensitivity: SensitivityDto;
  hint: GridHintDto;
  stats: GroupStatsDto;
}

/** What Split Here did. */
export interface SplitHereDto {
  split: boolean;
  regions: number;
  groupIndex: number;
  elapsedMs: number;
  report: GroupingDto;
}

/** The overlay's backdrop. */
export interface SheetPreviewDto {
  /** Base64 PNG (no data-URL prefix). */
  png: string;
  width: number;
  height: number;
  sheetWidth: number;
  sheetHeight: number;
}

/** Per-group review state, in group order. */
export type GroupStatus = "ok" | WarningKind;

/** The colour one group is drawn with, and the stroke a warning adds to it. */
export const GROUP_STATUS_COLORS: Record<GroupStatus | "selected" | "marquee", string> = {
  ok: "#38bdf8",
  spansMultipleCells: "#fb923c",
  restoredFromMerge: "#f43f5e",
  selected: "#a3e635",
  marquee: "#e879f9",
};

/**
 * Slider definitions — the same envelope the native side enforces
 * (`SensitivityParams::RANGES`); the UI clamps, the backend rejects, and the
 * two must agree or the sliders can produce requests that always fail.
 */
export interface SliderSpec {
  key: keyof SensitivityDto;
  label: string;
  min: number;
  max: number;
  step: number;
  /** How the value is rendered next to the slider. */
  format: (v: number) => string;
}

export const SENSITIVITY_SLIDERS: SliderSpec[] = [
  {
    key: "mergeGapFrac",
    label: "merge distance",
    min: 0.05,
    max: 1.0,
    step: 0.05,
    format: (v) => `${v.toFixed(2)}×h`,
  },
  {
    key: "mergeAreaRatio",
    label: "merge area ceiling",
    min: 1.0,
    max: 8.0,
    step: 0.05,
    format: (v) => `${v.toFixed(2)}×`,
  },
  {
    key: "noiseMinArea",
    label: "speckle area",
    min: 0,
    max: 512,
    step: 1,
    format: (v) => `${Math.round(v)} px`,
  },
  {
    key: "gridRegularityMin",
    label: "lattice regularity",
    min: 0,
    max: 1,
    step: 0.01,
    format: (v) => v.toFixed(2),
  },
];

/** Clamps every knob into its slider range (and drops NaN). */
export function clampSensitivity(s: SensitivityDto): SensitivityDto {
  const out = { ...s };
  for (const spec of SENSITIVITY_SLIDERS) {
    const raw = out[spec.key];
    const value = Number.isFinite(raw) ? raw : spec.min;
    out[spec.key] = Math.min(spec.max, Math.max(spec.min, value));
  }
  return out;
}

/** The sliders a request should be sent with. */
export function sliderRequest(s: SensitivityDto, key: keyof SensitivityDto, value: number): SensitivityDto {
  return clampSensitivity({ ...s, [key]: value });
}

/**
 * Per-group status, in group order: the warning layer of the overlay.
 *
 * `spansMultipleCells` wins over `restoredFromMerge` when a group somehow
 * carries both, so the stronger warning is the one drawn.
 */
export function groupStatuses(dto: GroupingDto): GroupStatus[] {
  const out: GroupStatus[] = dto.groups.map(() => "ok");
  for (const w of dto.warnings) {
    const i = Math.round(w.group);
    if (i < 0 || i >= out.length) continue;
    if (w.kind === "spansMultipleCells") out[i] = "spansMultipleCells";
    else if (out[i] === "ok") out[i] = "restoredFromMerge";
  }
  return out;
}

/** Golden-angle palette: adjacent groups never share a hue. */
export function groupColor(index: number): string {
  const hue = (index * 137.508) % 360;
  return `hsl(${hue.toFixed(1)} 70% 58%)`;
}

/** `3 · 24×24` — the label drawn on a group. */
export function groupLabel(dto: GroupingDto, index: number): string {
  const g = dto.groups[index];
  if (!g) return "";
  return `${index} · ${g.bbox[2]}×${g.bbox[3]}`;
}

/** Sheet pixels → preview pixels. */
export function previewScale(p: SheetPreviewDto): number {
  return p.sheetWidth > 0 ? p.width / p.sheetWidth : 1;
}

/** A point in preview pixels → sheet pixels (what the backend commands take). */
export function previewToSheet(p: SheetPreviewDto, x: number, y: number): [number, number] {
  const s = previewScale(p);
  return [Math.round(x / s), Math.round(y / s)];
}

/** Do two sheet-space boxes overlap? (Touching edges do not count.) */
export function boxesIntersect(a: Bbox, b: Bbox): boolean {
  return a[0] < b[0] + b[2] && b[0] < a[0] + a[2] && a[1] < b[1] + b[3] && b[1] < a[1] + a[3];
}

/** Indices of the groups a marquee box hits — what "Group Selected" sends. */
export function marqueeSelection(groups: GroupDto[], box: Bbox): number[] {
  const out: number[] = [];
  groups.forEach((g, i) => {
    if (boxesIntersect(g.bbox, box)) out.push(i);
  });
  return out;
}

/** Smallest box containing all of `boxes`; `null` when empty. */
export function unionBbox(boxes: Bbox[]): Bbox | null {
  if (boxes.length === 0) return null;
  let [x0, y0, x1, y1] = [Infinity, Infinity, -Infinity, -Infinity];
  for (const [x, y, w, h] of boxes) {
    x0 = Math.min(x0, x);
    y0 = Math.min(y0, y);
    x1 = Math.max(x1, x + w);
    y1 = Math.max(y1, y + h);
  }
  return [x0, y0, x1 - x0, y1 - y0];
}

/** Normalizes a drag (any direction) into a sheet-space box of ≥ 1×1 px. */
export function dragBox(
  start: [number, number],
  end: [number, number],
  sheetWidth: number,
  sheetHeight: number,
): Bbox {
  const x0 = Math.max(0, Math.min(start[0], end[0]));
  const y0 = Math.max(0, Math.min(start[1], end[1]));
  const x1 = Math.min(sheetWidth, Math.max(start[0], end[0]));
  const y1 = Math.min(sheetHeight, Math.max(start[1], end[1]));
  return [
    Math.round(x0),
    Math.round(y0),
    Math.max(1, Math.round(x1 - x0)),
    Math.max(1, Math.round(y1 - y0)),
  ];
}

/** True when a drag is small enough to count as a click (Split Here). */
export function isClick(start: [number, number], end: [number, number], slopPx = 4): boolean {
  return Math.abs(end[0] - start[0]) <= slopPx && Math.abs(end[1] - start[1]) <= slopPx;
}

/** `Split Here: 2 regions in 0.4 ms` — the Split Here result line. */
export function splitHereSummary(dto: SplitHereDto): string {
  if (!dto.split) return `Split Here: refused (no watershed structure) · ${dto.elapsedMs.toFixed(2)} ms`;
  return `Split Here: ${dto.regions} regions · ${dto.elapsedMs.toFixed(2)} ms`;
}

/** `100 icons · 92% confidence · 3 need review · grouped in 0.31 s`. */
export function overlaySummary(dto: GroupingDto): string {
  const review = dto.reviewGroups === 0 ? "none need review" : `${dto.reviewGroups} need review`;
  return (
    `${dto.groups.length} icons · ${Math.round(dto.confidence * 100)}% confidence · ${review} · ` +
    `${dto.elapsedMs.toFixed(0)} ms`
  );
}
