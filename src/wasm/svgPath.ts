/**
 * Bridge from the vectorizer's output to the editor's document model.
 *
 * Phase 4A loads icons that the existing Rust tracer already produced: the
 * renderer hands the UI an SVG string, and that string becomes node geometry.
 * The parser therefore covers exactly the subset `vtracer` emits — `M`, `L`,
 * `H`, `V`, `C` and `Z`, absolute or relative — and **throws** on anything else
 * instead of guessing: a silently mis-parsed outline would be an editing bug the
 * user cannot see coming.
 *
 * (Longer term this parsing belongs in Rust beside the tracer; keeping it here
 * for 4A is recorded as a deviation in `docs/ARCHITECTURE.md` §8.)
 */

import { KIND, type Point, type Rgba, type Segment, type Subpath } from "./abi";

/** One `<path>` element of an SVG. */
export interface SvgPathElement {
  /** The `d` attribute. */
  d: string;
  /** The `fill` attribute as RGBA, when it is a hex colour. */
  fill: Rgba | null;
}

/** The commands `parsePathData` understands. */
const COMMANDS = "MmLlHhVvCcZz";

/** Parses `#rgb`, `#rrggbb` and `#rrggbbaa` (with or without the `#`). */
export function hexToRgba(value: string): Rgba | null {
  const hex = value.trim().replace(/^#/, "");
  if (!/^[0-9a-f]{3,8}$/i.test(hex)) return null;
  const expand = (part: string): number => parseInt(part.length === 1 ? part + part : part, 16);
  switch (hex.length) {
    case 3:
      return [expand(hex[0]), expand(hex[1]), expand(hex[2]), 255];
    case 4:
      return [expand(hex[0]), expand(hex[1]), expand(hex[2]), expand(hex[3])];
    case 6:
      return [
        expand(hex.slice(0, 2)),
        expand(hex.slice(2, 4)),
        expand(hex.slice(4, 6)),
        255,
      ];
    case 8:
      return [
        expand(hex.slice(0, 2)),
        expand(hex.slice(2, 4)),
        expand(hex.slice(4, 6)),
        expand(hex.slice(6, 8)),
      ];
    default:
      return null;
  }
}

/** Extracts every `<path>` of an SVG string, in document order. */
export function readSvgPaths(svg: string): SvgPathElement[] {
  const out: SvgPathElement[] = [];
  const tag = /<path\b([^>]*)>/gi;
  for (let match = tag.exec(svg); match; match = tag.exec(svg)) {
    const attrs = match[1];
    const d = /\bd\s*=\s*"([^"]*)"/i.exec(attrs)?.[1] ?? /\bd\s*=\s*'([^']*)'/i.exec(attrs)?.[1];
    if (!d) continue;
    const fillText = /\bfill\s*=\s*"([^"]*)"/i.exec(attrs)?.[1] ?? /\bfill\s*=\s*'([^']*)'/i.exec(attrs)?.[1];
    out.push({ d, fill: fillText ? hexToRgba(fillText) : null });
  }
  return out;
}

interface Cursor {
  index: number;
  tokens: string[];
}

function number(cursor: Cursor): number {
  const token = cursor.tokens[cursor.index++];
  if (token === undefined) throw new SyntaxError("path data ends mid-command");
  const value = Number(token);
  if (!Number.isFinite(value)) throw new SyntaxError(`expected a number, got "${token}"`);
  return value;
}

function hasNumber(cursor: Cursor): boolean {
  const token = cursor.tokens[cursor.index];
  return token !== undefined && Number.isFinite(Number(token));
}

/**
 * Parses SVG path data into subpaths.
 *
 * @throws SyntaxError for an unsupported command letter (quadratic/arc/smooth
 * variants are not produced by the tracer) or malformed numbers.
 */
export function parsePathData(d: string): Subpath[] {
  // Letters are matched *generally* and then validated: a tokenizer that only
  // recognised the supported commands would silently drop an `A` or `Q` and
  // mis-parse the rest of the path instead of refusing it.
  const tokens = d.match(/[A-Za-z]|-?(?:\d+\.?\d*|\.\d+)(?:[eE][-+]?\d+)?/g);
  if (!tokens) return [];
  const cursor: Cursor = { index: 0, tokens };
  const subpaths: Subpath[] = [];
  let current: Subpath | null = null;
  let point: Point = { x: 0, y: 0 };
  let start: Point = { x: 0, y: 0 };
  let command = "";

  const open = (at: Point): Subpath => {
    current = { start: { x: at.x, y: at.y }, closed: false, segs: [] };
    subpaths.push(current);
    return current;
  };
  const requireOpen = (): Subpath => {
    if (!current) throw new SyntaxError("path data must start with a moveto");
    return current;
  };
  const line = (to: Point): Segment => ({ kind: KIND.LINE, to });
  const cubic = (c1: Point, c2: Point, to: Point): Segment => ({ kind: KIND.CUBIC, c1, c2, to });

  while (cursor.index < tokens.length) {
    const next = tokens[cursor.index];
    if (/^[A-Za-z]$/.test(next)) {
      if (!new RegExp(`^[${COMMANDS}]$`).test(next)) {
        throw new SyntaxError(`unsupported path command "${next}"`);
      }
      command = next;
      cursor.index += 1;
    } else if (!command) {
      throw new SyntaxError(`path data starts with "${next}" instead of a command`);
    } else if (command === "M") {
      command = "L"; // implicit lineto after the first moveto pair
    } else if (command === "m") {
      command = "l";
    }

    switch (command) {
      case "M":
      case "m": {
        const relative = command === "m";
        const at = { x: number(cursor), y: number(cursor) };
        point = relative ? { x: point.x + at.x, y: point.y + at.y } : at;
        start = { ...point };
        open(point);
        break;
      }
      case "L":
      case "l": {
        const relative = command === "l";
        const to = { x: number(cursor), y: number(cursor) };
        const target = relative ? { x: point.x + to.x, y: point.y + to.y } : to;
        requireOpen().segs.push(line(target));
        point = target;
        break;
      }
      case "H":
      case "h": {
        const relative = command === "h";
        const x = number(cursor);
        const target = { x: relative ? point.x + x : x, y: point.y };
        requireOpen().segs.push(line(target));
        point = target;
        break;
      }
      case "V":
      case "v": {
        const relative = command === "v";
        const y = number(cursor);
        const target = { x: point.x, y: relative ? point.y + y : y };
        requireOpen().segs.push(line(target));
        point = target;
        break;
      }
      case "C":
      case "c": {
        const relative = command === "c";
        const read = (): Point => {
          const raw = { x: number(cursor), y: number(cursor) };
          return relative ? { x: point.x + raw.x, y: point.y + raw.y } : raw;
        };
        const c1 = read();
        const c2 = read();
        const to = read();
        requireOpen().segs.push(cubic(c1, c2, to));
        point = to;
        break;
      }
      case "Z":
      case "z": {
        requireOpen().closed = true;
        point = { ...start };
        break;
      }
      default:
        throw new SyntaxError(`unsupported path command "${command}"`);
    }

    // A 'Z' takes no arguments; every other command consumes at least one number.
    if (command !== "Z" && command !== "z" && !hasNumber(cursor) && cursor.index < tokens.length) {
      const stray = tokens[cursor.index];
      if (/^[A-Za-z]$/.test(stray)) continue; // the next command's letter
      throw new SyntaxError(`unexpected "${stray}" in path data`);
    }
  }
  return subpaths;
}
