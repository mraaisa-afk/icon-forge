// Minimal deterministic PNG codec + JPEG helpers for the corpus tooling.
// Encoding: color type 0 (8-bit gray) / 2 (8-bit RGB), filter 0 per row.
// Container bytes are NOT part of the determinism contract (only decoded
// pixels and the JSON sidecars are — see tools/gen-corpus docs).
import zlib from "node:zlib";
import fs from "node:fs";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const jpeg = require("jpeg-js");

const CRC_TABLE = (() => {
  const t = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c >>> 0;
  }
  return t;
})();

function crc32(buf) {
  let c = 0xffffffff;
  for (let i = 0; i < buf.length; i++) c = CRC_TABLE[(c ^ buf[i]) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

function chunk(type, data) {
  const out = Buffer.alloc(12 + data.length);
  out.writeUInt32BE(data.length, 0);
  out.write(type, 4, "ascii");
  data.copy(out, 8);
  out.writeUInt32BE(crc32(out.subarray(4, 8 + data.length)), 8 + data.length);
  return out;
}

/// px: Buffer/Uint8Array of w*h (gray) or w*h*3 (rgb) bytes.
export function encodePng(path, w, h, colorType, px) {
  const channels = colorType === 2 ? 3 : 1;
  const stride = w * channels;
  const raw = Buffer.alloc((stride + 1) * h);
  for (let y = 0; y < h; y++) {
    raw[y * (stride + 1)] = 0; // filter: none
    for (let x = 0; x < stride; x++) raw[y * (stride + 1) + 1 + x] = px[y * stride + x];
  }
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0);
  ihdr.writeUInt32BE(h, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = colorType;
  ihdr[10] = 0; // deflate
  ihdr[11] = 0; // filter
  ihdr[12] = 0; // interlace
  const sig = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
  const idat = zlib.deflateSync(raw, { level: 9 });
  fs.writeFileSync(
    path,
    Buffer.concat([sig, chunk("IHDR", ihdr), chunk("IDAT", idat), chunk("IEND", Buffer.alloc(0))])
  );
}

export function decodePng(path) {
  const buf = fs.readFileSync(path);
  if (buf.readUInt32BE(0) !== 0x89504e47) throw new Error(`not a PNG: ${path}`);
  let off = 8;
  let w = 0;
  let h = 0;
  let colorType = 0;
  const idats = [];
  while (off + 8 <= buf.length) {
    const len = buf.readUInt32BE(off);
    const type = buf.toString("ascii", off + 4, off + 8);
    const data = buf.subarray(off + 8, off + 8 + len);
    if (type === "IHDR") {
      w = data.readUInt32BE(0);
      h = data.readUInt32BE(4);
      if (data[8] !== 8) throw new Error("unsupported bit depth");
      colorType = data[9];
      if (data[12] !== 0) throw new Error("interlaced PNG unsupported");
    } else if (type === "IDAT") {
      idats.push(Buffer.from(data));
    } else if (type === "IEND") {
      break;
    }
    off += 12 + len;
  }
  if (colorType !== 0 && colorType !== 2) throw new Error(`unsupported color type ${colorType}`);
  const channels = colorType === 2 ? 3 : 1;
  const stride = w * channels;
  const raw = zlib.inflateSync(Buffer.concat(idats));
  const px = Buffer.alloc(w * h * channels);
  let prev = Buffer.alloc(stride);
  for (let y = 0; y < h; y++) {
    const filter = raw[y * (stride + 1)];
    const row = raw.subarray(y * (stride + 1) + 1, (y + 1) * (stride + 1));
    const cur = Buffer.alloc(stride);
    for (let i = 0; i < stride; i++) {
      const a = i >= channels ? cur[i - channels] : 0;
      const b = prev[i];
      const c = i >= channels ? prev[i - channels] : 0;
      let v = row[i];
      switch (filter) {
        case 0: break;
        case 1: v = (v + a) & 0xff; break;
        case 2: v = (v + b) & 0xff; break;
        case 3: v = (v + ((a + b) >> 1)) & 0xff; break;
        case 4: {
          const p = a + b - c;
          const pa = Math.abs(p - a);
          const pb = Math.abs(p - b);
          const pc = Math.abs(p - c);
          const pred = pa <= pb && pa <= pc ? a : pb <= pc ? b : c;
          v = (v + pred) & 0xff;
          break;
        }
        default: throw new Error(`bad filter ${filter}`);
      }
      cur[i] = v;
    }
    cur.copy(px, y * stride);
    prev = cur;
  }
  return { width: w, height: h, colorType, px, channels };
}

/// Encode a gray (L8) buffer as JPEG via jpeg-js (RGBA input).
export function encodeJpegGray(path, w, h, gray, quality = 85) {
  const rgba = Buffer.alloc(w * h * 4);
  for (let i = 0; i < w * h; i++) {
    rgba[i * 4] = gray[i];
    rgba[i * 4 + 1] = gray[i];
    rgba[i * 4 + 2] = gray[i];
    rgba[i * 4 + 3] = 255;
  }
  const out = jpeg.encode({ data: rgba, width: w, height: h }, quality);
  fs.writeFileSync(path, out.data);
}

export function decodeImage(path) {
  if (path.endsWith(".jpg") || path.endsWith(".jpeg")) {
    const img = jpeg.decode(fs.readFileSync(path), { useTArray: true, formatAsRGBA: true });
    return { width: img.width, height: img.height, channels: 4, px: img.data };
  }
  return decodePng(path);
}

/// Luma (Rec.601) of a decoded image, matching the spike's `to_luma8` closely
/// enough for verification (threshold margins are >= 60 luma everywhere).
export function toLuma(img) {
  const out = new Float32Array(img.width * img.height);
  for (let i = 0; i < out.length; i++) {
    if (img.channels === 1) {
      out[i] = img.px[i];
    } else {
      const r = img.px[i * img.channels];
      const g = img.px[i * img.channels + 1];
      const b = img.px[i * img.channels + 2];
      out[i] = 0.299 * r + 0.587 * g + 0.114 * b;
    }
  }
  return out;
}
