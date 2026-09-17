/**
 * Pure geometry for the editor canvas.
 *
 * Kept out of the component so the mapping between screen pixels and document
 * units is testable without a DOM: getting that wrong means clicks select the
 * wrong icon, which is exactly the kind of bug a canvas test would otherwise
 * miss.
 */

import type { Box } from "../../wasm/abi";

/** Where the document sits inside the canvas, and how big one unit is. */
export interface View {
  /** Document units per canvas pixel. */
  scale: number;
  offsetX: number;
  offsetY: number;
}

/** Fits a document into a canvas box, leaving a margin all round. */
export function fitView(
  canvasWidth: number,
  canvasHeight: number,
  docWidth: number,
  docHeight: number,
  margin: number,
): View {
  if (docWidth <= 0 || docHeight <= 0 || canvasWidth <= 0 || canvasHeight <= 0) {
    return { scale: 1, offsetX: 0, offsetY: 0 };
  }
  const scale = Math.min(
    (canvasWidth - margin * 2) / docWidth,
    (canvasHeight - margin * 2) / docHeight,
  );
  const fit = scale > 0 ? scale : 1;
  return {
    scale: fit,
    offsetX: (canvasWidth - docWidth * fit) / 2,
    offsetY: (canvasHeight - docHeight * fit) / 2,
  };
}

/** Screen point (relative to the canvas box) to document point. */
export function toDocPoint(
  x: number,
  y: number,
  view: View,
): { x: number; y: number } {
  return { x: (x - view.offsetX) / view.scale, y: (y - view.offsetY) / view.scale };
}

/** Document point to screen point (relative to the canvas box). */
export function toScreenPoint(x: number, y: number, view: View): { x: number; y: number } {
  return { x: x * view.scale + view.offsetX, y: y * view.scale + view.offsetY };
}

/** Normalises two drag corners into a box (the ABI accepts corners in any order). */
export function marqueeBox(
  a: { x: number; y: number },
  b: { x: number; y: number },
): Box {
  return {
    x0: Math.min(a.x, b.x),
    y0: Math.min(a.y, b.y),
    x1: Math.max(a.x, b.x),
    y1: Math.max(a.y, b.y),
  };
}

/** What a pointer-down on the canvas should start. */
export type Gesture =
  | { kind: "marquee" }
  | { kind: "move"; selectFirst: "none" | "only" | "toggle" };

/**
 * Decides a gesture from what is under the pointer.
 *
 * A grab on empty space sweeps a marquee; a grab on a node moves the selection,
 * selecting the node first if it was not part of it (so dragging always moves
 * what is under the cursor). Grabbing a node that is already selected never
 * changes the selection, so a multi-node drag stays intact.
 */
export function gestureFor(
  hit: number,
  selection: readonly number[],
  additive: boolean,
): Gesture {
  if (hit === 0) return { kind: "marquee" };
  if (selection.includes(hit)) return { kind: "move", selectFirst: "none" };
  return { kind: "move", selectFirst: additive ? "toggle" : "only" };
}

/** Whether a move gesture passed the click threshold (document units). */
export const DRAG_THRESHOLD = 0.5;

export function isDrag(dx: number, dy: number): boolean {
  return Math.hypot(dx, dy) > DRAG_THRESHOLD;
}
