import { describe, expect, it } from "vitest";

import {
  decodeNodeRecords,
  decodePath,
  decodeText,
  encodeCommand,
  encodeDoc,
  encodePath,
  ERR,
  FEATURE,
  f2w,
  KIND,
  MAX_NODES,
  NODE_RECORD_HEADER,
  OP,
  packRgba,
  unpackRgba,
  w2f,
  type NodeSpec,
  type Subpath,
} from "./abi";

function square(x: number, y: number, size: number): Subpath {
  return {
    start: { x, y },
    closed: true,
    segs: [
      { kind: KIND.LINE, to: { x: x + size, y } },
      { kind: KIND.LINE, to: { x: x + size, y: y + size } },
      { kind: KIND.LINE, to: { x, y: y + size } },
    ],
  };
}

function arc(x: number, y: number): Subpath {
  return {
    start: { x, y },
    closed: false,
    segs: [
      {
        kind: KIND.CUBIC,
        c1: { x, y: y + 10 },
        c2: { x: x + 10, y: y + 10 },
        to: { x: x + 10, y },
      },
    ],
  };
}

const nodes: NodeSpec[] = [
  { id: 1, m: [1, 0, 0, 1, 0, 0], fill: [255, 0, 128, 255], visible: true, path: [square(10, 10, 20)] },
  { id: 2, m: [0.866, 0.5, -0.5, 0.866, 50, 20], fill: [0, 255, 0, 200], visible: true, path: [arc(0, 0)] },
  { id: 3, m: [1, 0, 0, 1, 5, 5], fill: [1, 2, 3, 4], visible: false, path: [square(0, 0, 4), arc(1, 1)] },
];

describe("abi constants", () => {
  it("keeps the feature numbers the Rust side defines", () => {
    expect(FEATURE.DOC_LOAD).toBe(1);
    expect(FEATURE.NODE_SYNC).toBe(4);
    expect(FEATURE.APPLY_SPEC).toBe(20);
    expect(FEATURE.UNDO).toBe(21);
    expect(FEATURE.REDO).toBe(22);
    expect(FEATURE.ERROR).toBe(32);
    expect(ERR.NO_DOCUMENT).toBe(2);
    expect(ERR.NO_HISTORY).toBe(5);
    expect(NODE_RECORD_HEADER).toBe(10);
  });

  it("round-trips f32 bits and packed fills", () => {
    // The codec's contract is bit fidelity: whatever f32 bit pattern is packed
    // comes back unchanged. (A decimal like 0.001 is rounded *before* packing,
    // which is exactly what the Rust side will store.)
    for (const word of [0, 1, 0x3f800000, 0x3a83126f, 0x7f7fffff, 0xc0200000]) {
      expect(f2w(w2f(word))).toBe(word);
    }
    for (const value of [0, 1, -1, 0.5, 1e-3, -2.5]) {
      expect(w2f(f2w(value))).toBe(Math.fround(value));
    }
    expect(packRgba([255, 0, 128, 255])).toBe(0xff0080ff);
    expect(unpackRgba(0xff0080ff)).toEqual([255, 0, 128, 255]);
  });
});

describe("document blob", () => {
  it("encodes the header the Rust decoder expects", () => {
    const words = encodeDoc(200, 120, nodes);
    expect(words.length).toBeGreaterThan(4);
    expect(w2f(words[0])).toBe(200);
    expect(w2f(words[1])).toBe(120);
    expect(words[2]).toBe(3);
    expect(words[3]).toBe(words.length);
  });

  it("round-trips through a node-record decode", () => {
    const doc = encodeDoc(200, 120, nodes);
    // Load the records back the way NODE_SYNC presents them: header + path.
    const records = nodes.map((node) => {
      const path = encodePath(node.path);
      return [node.id, ...node.m.map(f2w), packRgba(node.fill), node.visible ? 1 : 0, path.length, ...path];
    });
    const flat = Uint32Array.from(records.flat());
    const back = decodeNodeRecords(flat, 0, nodes.length);
    expect(back).toHaveLength(3);
    expect(back[0].path).toEqual(nodes[0].path);
    expect(back[1].path).toEqual(nodes[1].path);
    expect(back[1].m.map((v) => Math.fround(v))).toEqual(nodes[1].m.map((v) => Math.fround(v)));
    expect(back[2].visible).toBe(false);
    expect(back[2].fill).toEqual([1, 2, 3, 4]);
    expect(back[2].path).toHaveLength(2);
    expect(doc[2]).toBe(3);
  });

  it("walks records from an offset and stops at the terminator", () => {
    const path = encodePath(nodes[0].path);
    const region = Uint32Array.from([
      0xffffffff, // some earlier call's leftovers
      0xffffffff,
      nodes[0].id,
      ...nodes[0].m.map(f2w),
      packRgba(nodes[0].fill),
      1,
      path.length,
      ...path,
      0, // terminator
    ]);
    expect(decodeNodeRecords(region, 2, 5)).toHaveLength(1);
  });

  it("rejects a node whose declared path length is a lie", () => {
    const path = encodePath(nodes[0].path);
    path[0] = 99; // the count word the record points at, not the header's length
    const record = Uint32Array.from([
      nodes[0].id,
      ...nodes[0].m.map(f2w),
      packRgba(nodes[0].fill),
      1,
      encodePath(nodes[0].path).length,
      ...path,
    ]);
    expect(() => decodeNodeRecords(record, 0, 1)).toThrow(RangeError);
  });

  it("refuses documents the module would refuse", () => {
    expect(() => encodeDoc(0, 10, [])).toThrow(RangeError);
    expect(() => encodeDoc(10, Number.NaN, [])).toThrow(RangeError);
    expect(() => encodeDoc(10, 10, [{ ...nodes[0], id: 0 }])).toThrow(RangeError);
    const tooMany = Array.from({ length: MAX_NODES + 1 }, (_, i) => ({ ...nodes[0], id: i + 1 }));
    expect(() => encodeDoc(10, 10, tooMany)).toThrow(RangeError);
  });

  it("rejects an unknown segment kind while decoding", () => {
    const words = Uint32Array.from([1, 0, 0, 1, 1, 0, 0, 0, 0, 0, 0]);
    words[5] = 7; // kind 7 does not exist
    expect(() => decodePath(words, 0)).toThrow(RangeError);
  });
});

describe("command specs", () => {
  it("lays every command out the way abi.rs decodes it", () => {
    expect(Array.from(encodeCommand({ kind: "translate", dx: 5, dy: -3 }))).toEqual([
      OP.TRANSLATE,
      f2w(5),
      f2w(-3),
    ]);
    expect(Array.from(encodeCommand({ kind: "scale", factor: 2, pivot: { x: 1, y: 2 } }))).toEqual([
      OP.SCALE,
      f2w(2),
      f2w(1),
      f2w(2),
    ]);
    expect(Array.from(encodeCommand({ kind: "rotate", degrees: 90, pivot: { x: 0, y: 0 } }))).toEqual([
      OP.ROTATE,
      f2w(90),
      0,
      0,
    ]);
    expect(Array.from(encodeCommand({ kind: "center" }))).toEqual([OP.CENTER]);
    expect(Array.from(encodeCommand({ kind: "fill", rgba: [9, 8, 7, 255] }))).toEqual([
      OP.SET_FILL,
      0x090807ff,
    ]);
    expect(Array.from(encodeCommand({ kind: "visible", to: false }))).toEqual([OP.SET_VISIBLE, 0]);
    expect(Array.from(encodeCommand({ kind: "reorder", up: true }))).toEqual([OP.REORDER, 1]);
    expect(Array.from(encodeCommand({ kind: "duplicate", dx: 1, dy: 2 }))).toEqual([
      OP.DUPLICATE,
      f2w(1),
      f2w(2),
    ]);
    expect(Array.from(encodeCommand({ kind: "delete" }))).toEqual([OP.DELETE]);
  });
});

describe("text channel", () => {
  it("unpacks little-endian UTF-8 words", () => {
    const word = (bytes: number[]): number =>
      bytes.reduce((acc, byte, i) => acc | (byte << (8 * i)), 0);
    const words = Uint32Array.from([
      word([..."move"].map((c) => c.charCodeAt(0))),
      0,
    ]);
    expect(decodeText(words, 4)).toBe("move");
  });
});
