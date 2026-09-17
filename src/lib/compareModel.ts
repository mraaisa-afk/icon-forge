/**
 * Pure model behind the A/B comparator: the seven §3.3-⑤ presets, split
 * ("wipe") geometry, score grading, and icon identity keys. No DOM and no
 * backend here — everything is unit-tested in plain node.
 */

export interface PresetInfo {
  /** §3.3-⑤ doc name — the wire format accepted by the backend. */
  name: string;
  /** Human label for the picker. */
  label: string;
}

/** The seven trace presets, in roadmap order (§3.3-⑤). */
export const PRESETS: readonly PresetInfo[] = [
  { name: "mono-fast", label: "Mono · fast" },
  { name: "mono-clean", label: "Mono · clean" },
  { name: "scan", label: "Scan / lineart" },
  { name: "flat-8", label: "Flat · 8" },
  { name: "flat-cutout", label: "Flat · cutout" },
  { name: "detailed", label: "Detailed" },
  { name: "pixel-art", label: "Pixel art" },
];

export type ViewMode = "a" | "split" | "b";

/** Tight icon bbox `(x, y, w, h)` inside the sheet. */
export type Bbox = readonly [number, number, number, number];

/** Stable identity for one (icon, preset) pair — the memo key. */
export function iconKey(bbox: Bbox, preset: string): string {
  return `${bbox[0]},${bbox[1]},${bbox[2]},${bbox[3]}@${preset}`;
}

export interface Clips {
  /** Visible A rect `[x, y, w, h]` — left of the divider. */
  a: readonly [number, number, number, number];
  /** Visible B rect — right of the divider. */
  b: readonly [number, number, number, number];
}

/**
 * Split geometry: `wipe` ∈ [0,1] is the fraction of the canvas width that
 * shows A (original); the remainder shows B (vectorized). Rounded to whole
 * pixels so the divider stays crisp, and clamped so both rects stay valid.
 */
export function wipeClips(wipe: number, w: number, h: number): Clips {
  const t = Math.min(1, Math.max(0, wipe));
  const x = Math.round(t * w);
  return { a: [0, 0, x, h], b: [x, 0, w - x, h] };
}

export interface Grade {
  label: "excellent" | "good" | "fair" | "poor";
  className: string;
}

/**
 * Composite (0.5·SSIM + 0.3·(1−MAE) + 0.2·IoU) → verdict colour.
 * Thresholds: excellent ≥ 0.97, good ≥ 0.92, fair ≥ 0.85.
 */
export function grade(composite: number): Grade {
  if (composite >= 0.97) return { label: "excellent", className: "text-emerald-400" };
  if (composite >= 0.92) return { label: "good", className: "text-lime-400" };
  if (composite >= 0.85) return { label: "fair", className: "text-amber-400" };
  return { label: "poor", className: "text-rose-400" };
}

/** Fixed-4 score formatting for the readout row. */
export function fmt(x: number): string {
  return x.toFixed(4);
}
