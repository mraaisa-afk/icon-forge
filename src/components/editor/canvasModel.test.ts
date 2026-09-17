import { describe, expect, it } from "vitest";

import { fitView, gestureFor, isDrag, marqueeBox, toDocPoint, toScreenPoint } from "./canvasModel";

describe("fitView", () => {
  it("fits a wide document to the width and centres it", () => {
    const view = fitView(200, 200, 100, 50, 10);
    expect(view.scale).toBeCloseTo(1.8, 6); // (200-20)/100
    expect(view.offsetX).toBeCloseTo(10, 6);
    expect(view.offsetY).toBeCloseTo((200 - 50 * 1.8) / 2, 6);
  });

  it("fits a tall document to the height", () => {
    const view = fitView(200, 200, 50, 100, 10);
    expect(view.scale).toBeCloseTo(1.8, 6);
    expect(view.offsetY).toBeCloseTo(10, 6);
  });

  it("falls back to whole pixels for an empty or impossible box", () => {
    expect(fitView(0, 0, 100, 100, 10)).toEqual({ scale: 1, offsetX: 0, offsetY: 0 });
    expect(fitView(200, 200, 0, 0, 10)).toEqual({ scale: 1, offsetX: 0, offsetY: 0 });
    // A margin bigger than the canvas would otherwise give a negative scale.
    expect(fitView(10, 10, 100, 100, 20).scale).toBe(1);
  });

  it("round-trips a point through screen and document space", () => {
    const view = fitView(640, 480, 256, 128, 12);
    const doc = toDocPoint(300, 200, view);
    const screen = toScreenPoint(doc.x, doc.y, view);
    expect(screen.x).toBeCloseTo(300, 6);
    expect(screen.y).toBeCloseTo(200, 6);
  });

  it("maps the document's top-left corner to the margin", () => {
    const view = fitView(200, 200, 100, 100, 10);
    const corner = toScreenPoint(0, 0, view);
    expect(corner).toEqual({ x: view.offsetX, y: view.offsetY });
    expect(toDocPoint(corner.x, corner.y, view)).toEqual({ x: 0, y: 0 });
  });
});

describe("marqueeBox", () => {
  it("normalises corners dragged in any direction", () => {
    expect(marqueeBox({ x: 10, y: 20 }, { x: 4, y: 30 })).toEqual({
      x0: 4,
      y0: 20,
      x1: 10,
      y1: 30,
    });
  });
});

describe("gestureFor", () => {
  it("sweeps on empty space", () => {
    expect(gestureFor(0, [1, 2], false)).toEqual({ kind: "marquee" });
    expect(gestureFor(0, [], true)).toEqual({ kind: "marquee" });
  });

  it("moves an already-selected node without touching the selection", () => {
    expect(gestureFor(2, [1, 2], false)).toEqual({ kind: "move", selectFirst: "none" });
  });

  it("selects an unselected node first, replacing or toggling per the modifier", () => {
    expect(gestureFor(3, [1, 2], false)).toEqual({ kind: "move", selectFirst: "only" });
    expect(gestureFor(3, [1, 2], true)).toEqual({ kind: "move", selectFirst: "toggle" });
  });
});

describe("isDrag", () => {
  it("separates a click from a nudge", () => {
    expect(isDrag(0, 0)).toBe(false);
    expect(isDrag(0.3, 0.3)).toBe(false);
    expect(isDrag(1, 0)).toBe(true);
  });
});
