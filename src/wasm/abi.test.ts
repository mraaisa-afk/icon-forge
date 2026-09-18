import { describe, expect, it } from "vitest";

import {
  decodeNodeRecords,
  decodePath,
  decodeSnapResult,
  decodeText,
  encodeCommand,
  encodeDoc,
  encodePath,
  encodeSnapRequest,
  encodeSvgSpec,
  ERR,
  FEATURE,
  f2w,
  GUIDE_KIND,
  GUIDE_WORDS,
  KIND,
  MAX_NODES,
  MAX_SNAP_GUIDES,
  NODE_RECORD_HEADER,
  OP,
  packRgba,
  PREVIEW,
  SNAP,
  SNAP_ANSWER_WORDS,
  unpackRgba,
  w2f,
  type NodeSpec,
  type Subpath,
} from "./abi";

/** Builds a node-record region the way `NODE_SYNC` writes one. */
function recordRegion(nodes: readonly NodeSpec[]): Uint32Array {
  const words: number[] = [];
  for (const node of nodes) {
    const path = encodePath(node.path);
    words.push(
      node.id,
      ...node.m.map(f2w),
      packRgba(node.fill),
      node.visible ? 1 : 0,
      node.group ?? 0,
      path.length,
      ...path,
    );
  }
  return Uint32Array.from(words);
}

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
    expect(NODE_RECORD_HEADER).toBe(11);
    // 4B: the group word, the snap/preview features and the five new commands.
    expect(FEATURE.SNAP).toBe(37);
    expect(FEATURE.PREVIEW).toBe(38);
    expect(OP.GROUP).toBe(10);
    expect(OP.UNGROUP).toBe(11);
    expect(OP.ARRANGE).toBe(12);
    expect(OP.ALIGN).toBe(13);
    expect(OP.SCALE_XY).toBe(14);
    expect(SNAP.CANVAS).toBe(1);
    expect(SNAP.NODES).toBe(2);
    expect(SNAP.GRID).toBe(4);
    expect(GUIDE_WORDS).toBe(5);
    expect(MAX_SNAP_GUIDES).toBe(8);
    expect(PREVIEW.SET).toBe(0);
    expect(PREVIEW.CLEAR).toBe(1);
    expect(SNAP_ANSWER_WORDS).toBe(3 + MAX_SNAP_GUIDES * GUIDE_WORDS + 1);
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
    const flat = recordRegion(nodes);
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
    const region = Uint32Array.from([
      0xffffffff, // some earlier call's leftovers
      0xffffffff,
      ...recordRegion([nodes[0]]),
      0, // terminator
    ]);
    expect(decodeNodeRecords(region, 2, 5)).toHaveLength(1);
  });

  it("carries a group id through the record", () => {
    const flat = recordRegion([{ ...nodes[0], group: 7 }, nodes[1]]);
    const back = decodeNodeRecords(flat, 0, 2);
    expect(back[0].group).toBe(7);
    expect(back[1].group).toBe(0);
    // …and the encoder writes the same slot.
    const doc = encodeDoc(200, 120, [{ ...nodes[0], group: 7 }]);
    expect(doc[4 + NODE_RECORD_HEADER - 1]).toBe(encodePath(nodes[0].path).length);
    expect(doc[4 + 9]).toBe(7);
  });

  it("rejects a node whose declared path length is a lie", () => {
    const path = encodePath(nodes[0].path);
    path[0] = 99; // the count word the record points at, not the header's length
    const record = Uint32Array.from([
      nodes[0].id,
      ...nodes[0].m.map(f2w),
      packRgba(nodes[0].fill),
      1,
      0,
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

  it("lays out the 4B commands too", () => {
    expect(
      Array.from(encodeCommand({ kind: "scaleXY", sx: 2, sy: 0.5, pivot: { x: 3, y: 4 } })),
    ).toEqual([OP.SCALE_XY, f2w(2), f2w(0.5), f2w(3), f2w(4)]);
    expect(Array.from(encodeCommand({ kind: "arrange", to: "front" }))).toEqual([OP.ARRANGE, 0]);
    expect(Array.from(encodeCommand({ kind: "arrange", to: "back" }))).toEqual([OP.ARRANGE, 1]);
    // The edge and frame numbers are the wire values `AlignEdge`/`AlignFrame` define.
    const edges = ["left", "hcenter", "right", "top", "vcenter", "bottom"] as const;
    edges.forEach((edge, raw) => {
      expect(
        Array.from(encodeCommand({ kind: "align", frame: "selection", edge })),
      ).toEqual([OP.ALIGN, 0, raw]);
      expect(Array.from(encodeCommand({ kind: "align", frame: "canvas", edge }))).toEqual([
        OP.ALIGN,
        1,
        raw,
      ]);
    });
    expect(Array.from(encodeCommand({ kind: "group" }))).toEqual([OP.GROUP]);
    expect(Array.from(encodeCommand({ kind: "ungroup" }))).toEqual([OP.UNGROUP]);
  });

  it("lays out the 4C commands too", () => {
    // Point edits carry the address as words, then the target as f32 bits.
    expect(
      Array.from(
        encodeCommand({
          kind: "movePoint",
          node: 4,
          at: { of: "vertex", subpath: 1, vertex: 2 },
          to: { x: 7, y: -8 },
        }),
      ),
    ).toEqual([OP.MOVE_POINT, 4, 1, 2, f2w(7), f2w(-8)]);
    expect(
      Array.from(
        encodeCommand({
          kind: "moveHandle",
          node: 4,
          at: { of: "handle", subpath: 0, segment: 3, handle: "c2" },
          to: { x: 1, y: 2 },
        }),
      ),
    ).toEqual([OP.MOVE_HANDLE, 4, 0, 3, 1, f2w(1), f2w(2)]);
    expect(
      Array.from(
        encodeCommand({
          kind: "insertPoint",
          node: 9,
          at: { of: "segment", subpath: 0, segment: 1, t: 0.25 },
        }),
      ),
    ).toEqual([OP.INSERT_POINT, 9, 0, 1, f2w(0.25)]);
    expect(
      Array.from(
        encodeCommand({ kind: "deletePoint", node: 9, at: { of: "vertex", subpath: 2, vertex: 0 } }),
      ),
    ).toEqual([OP.DELETE_POINT, 9, 2, 0]);
    // Segment kinds and boolean ops are the raw numbers the Rust enums define.
    expect(
      Array.from(
        encodeCommand({
          kind: "setSegment",
          node: 9,
          at: { of: "segment", subpath: 0, segment: 1, t: 0.5 },
          to: "cubic",
        }),
      ),
    ).toEqual([OP.SET_SEGMENT, 9, 0, 1, 1]);
    const ops = ["union", "subtract", "intersect", "exclude"] as const;
    ops.forEach((op, raw) => {
      expect(Array.from(encodeCommand({ kind: "boolean", op }))).toEqual([OP.BOOLEAN, raw]);
    });
  });
});

describe("svg spec", () => {
  it("packs the header the module reads, and the text four bytes to a word", () => {
    const words = encodeSvgSpec("<svg/>", 7, [1, 0, 0, 1, 5, 6], [1, 2, 3, 255]);
    // id, placement (f32 bits), fill, byte length, then the packed text.
    expect(words.slice(0, 8)).toEqual([7, f2w(1), f2w(0), f2w(0), f2w(1), f2w(5), f2w(6), 0x010203ff]);
    expect(words[8]).toBe(6);
    expect(words).toHaveLength(9 + 2);
    // The module reads text back with the same little-endian packing.
    expect(decodeText(Uint32Array.from(words.slice(9)), 6)).toBe("<svg/>");
  });

  it("packs multi-byte characters and pads the last word", () => {
    const text = "<svg>…</svg>";
    const words = encodeSvgSpec(text, 0, [1, 0, 0, 1, 0, 0], [0, 0, 0, 0]);
    const length = new TextEncoder().encode(text).length;
    expect(words[8]).toBe(length);
    expect(words.length).toBe(9 + Math.ceil(length / 4));
    expect(decodeText(Uint32Array.from(words.slice(9)), length)).toBe(text);
  });
});

describe("snap exchange", () => {
  it("encodes a request as dx, dy, tolerance, flags, grid step", () => {
    const words = encodeSnapRequest(7, -3, 6, SNAP.NODES | SNAP.GRID, 8);
    expect(Array.from(words)).toEqual([f2w(7), f2w(-3), f2w(6), SNAP.NODES | SNAP.GRID, f2w(8)]);
  });

  it("reads the corrected delta, the guides and the terminator", () => {
    const words = new Uint32Array(SNAP_ANSWER_WORDS);
    words[0] = f2w(7);
    words[1] = f2w(-2);
    words[2] = 2;
    words[3] = 0;
    words[4] = GUIDE_KIND.GRID;
    words[5] = f2w(72);
    words[6] = f2w(0);
    words[7] = f2w(120);
    words[8] = 1;
    words[9] = GUIDE_KIND.NODE_EDGE;
    words[10] = f2w(48);
    words[11] = f2w(10);
    words[12] = f2w(80);
    words[13] = 0;
    const result = decodeSnapResult(words);
    expect(result.dx).toBe(7);
    expect(result.dy).toBe(-2);
    expect(result.guides).toEqual([
      { axis: 0, kind: GUIDE_KIND.GRID, position: 72, from: 0, to: 120 },
      { axis: 1, kind: GUIDE_KIND.NODE_EDGE, position: 48, from: 10, to: 80 },
    ]);
  });

  it("refuses a guide list that is not terminated", () => {
    const words = new Uint32Array(SNAP_ANSWER_WORDS);
    words[2] = 1;
    words[3 + GUIDE_WORDS] = 0xdeadbeef;
    expect(() => decodeSnapResult(words)).toThrow(RangeError);
  });

  it("never reads more guides than the module can write", () => {
    const words = new Uint32Array(SNAP_ANSWER_WORDS);
    words[2] = MAX_SNAP_GUIDES + 5; // a corrupt count
    expect(decodeSnapResult(words).guides).toHaveLength(MAX_SNAP_GUIDES);
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
