import { describe, expect, it } from "vitest";

import { KIND } from "./abi";
import { hexToRgba, parsePathData, readSvgPaths } from "./svgPath";

// The shape of what the Rust tracer emits: absolute M/L/C/Z, one path per colour.
const TRACED = `<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24">
<path d="M2 2 C2 14 22 14 22 2 L22 22 L2 22 Z" fill="#3b82f6"/>
<path d="M6 6 L10 6 L10 10 L6 10 Z" fill="#f97316" fill-opacity="0.5"/>
</svg>`;

describe("readSvgPaths", () => {
  it("finds every path with its fill", () => {
    const paths = readSvgPaths(TRACED);
    expect(paths).toHaveLength(2);
    expect(paths[0].fill).toEqual([0x3b, 0x82, 0xf6, 255]);
    expect(paths[1].fill).toEqual([0xf9, 0x73, 0x16, 255]);
  });

  it("ignores elements without path data and unknown fills", () => {
    const paths = readSvgPaths(`<svg><rect width="1" height="1"/><path fill="red"/><path d="M0 0Z"/></svg>`);
    expect(paths).toHaveLength(1);
    expect(paths[0].d).toBe("M0 0Z");
    expect(paths[0].fill).toBeNull();
  });
});

describe("hexToRgba", () => {
  it("accepts the short, long and alpha forms", () => {
    expect(hexToRgba("#abc")).toEqual([0xaa, 0xbb, 0xcc, 255]);
    expect(hexToRgba("aabbcc")).toEqual([0xaa, 0xbb, 0xcc, 255]);
    expect(hexToRgba("#aabbcc80")).toEqual([0xaa, 0xbb, 0xcc, 0x80]);
    expect(hexToRgba("#abcd")).toEqual([0xaa, 0xbb, 0xcc, 0xdd]);
    expect(hexToRgba("rebeccapurple")).toBeNull();
  });
});

describe("parsePathData", () => {
  it("parses the tracer's absolute M/C/L/Z subset", () => {
    const [first, second] = parsePathData(
      "M2 2 C2 14 22 14 22 2 L22 22 L2 22 Z M6 6 L10 6 L10 10 L6 10 Z",
    );
    expect(first.start).toEqual({ x: 2, y: 2 });
    expect(first.closed).toBe(true);
    expect(first.segs).toHaveLength(3);
    const cubic = first.segs[0];
    expect(cubic.kind).toBe(KIND.CUBIC);
    if (cubic.kind === KIND.CUBIC) {
      expect(cubic.c1).toEqual({ x: 2, y: 14 });
      expect(cubic.c2).toEqual({ x: 22, y: 14 });
      expect(cubic.to).toEqual({ x: 22, y: 2 });
    }
    expect(second.start).toEqual({ x: 6, y: 6 });
    expect(second.segs.map((s) => s.to)).toEqual([
      { x: 10, y: 6 },
      { x: 10, y: 10 },
      { x: 6, y: 10 },
    ]);
  });

  it("handles implicit linetos and the H/V shortcuts", () => {
    // Note the SVG rule this exercises: after `V5` the remaining numbers repeat
    // the *V* command, not a lineto — so the explicit `L` below is required.
    const [sub] = parsePathData("M0 0 1 1 H5 V5 L8 8");
    expect(sub.segs).toEqual([
      { kind: KIND.LINE, to: { x: 1, y: 1 } },
      { kind: KIND.LINE, to: { x: 5, y: 1 } },
      { kind: KIND.LINE, to: { x: 5, y: 5 } },
      { kind: KIND.LINE, to: { x: 8, y: 8 } },
    ]);
  });

  it("resolves relative commands against the running point", () => {
    const [sub] = parsePathData("m10 10 l5 0 v-5 h-5 c1 1 2 2 3 3 z");
    expect(sub.start).toEqual({ x: 10, y: 10 });
    expect(sub.closed).toBe(true);
    expect(sub.segs[0]).toEqual({ kind: KIND.LINE, to: { x: 15, y: 10 } });
    expect(sub.segs[1]).toEqual({ kind: KIND.LINE, to: { x: 15, y: 5 } });
    expect(sub.segs[2]).toEqual({ kind: KIND.LINE, to: { x: 10, y: 5 } });
    const last = sub.segs[3];
    expect(last.kind).toBe(KIND.CUBIC);
    if (last.kind === KIND.CUBIC) expect(last.to).toEqual({ x: 13, y: 8 });
  });

  it("keeps a closed subpath's closing edge implicit", () => {
    const [sub] = parsePathData("M0 0 L4 0 L4 4 Z");
    // The editor adds the closing edge itself: no duplicated start point.
    expect(sub.segs).toHaveLength(2);
    expect(sub.closed).toBe(true);
  });

  it("returns nothing for empty data", () => {
    expect(parsePathData("")).toEqual([]);
  });

  it("refuses commands it cannot represent faithfully", () => {
    expect(() => parsePathData("M0 0 A5 5 0 0 1 10 10")).toThrow(/unsupported/);
    expect(() => parsePathData("M0 0 Q5 5 10 10")).toThrow(/unsupported/);
    expect(() => parsePathData("L5 5")).toThrow(/moveto/);
    expect(() => parsePathData("M0")).toThrow(/ends mid-command/);
  });
});
