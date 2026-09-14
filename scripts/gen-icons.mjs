// Deterministic Icon Forge app icons (no external deps): draws the brand
// mark into RGBA buffers and encodes PNG (zlib via node:zlib) + a
// PNG-compressed .ico for Windows.
import { deflateSync } from "node:zlib";
import { writeFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..", "src-tauri", "icons");

function crc32(buf) {
  let c, table = crc32.table;
  if (!table) {
    table = crc32.table = new Int32Array(256);
    for (let n = 0; n < 256; n++) {
      c = n;
      for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
      table[n] = c;
    }
  }
  let crc = -1;
  for (let i = 0; i < buf.length; i++) crc = (crc >>> 8) ^ table[(crc ^ buf[i]) & 0xff];
  return (crc ^ -1) >>> 0;
}

function chunk(tag, data) {
  const out = Buffer.alloc(12 + data.length);
  out.writeUInt32BE(data.length, 0);
  out.write(tag, 4, "ascii");
  data.copy(out, 8);
  out.writeUInt32BE(crc32(Buffer.concat([Buffer.from(tag, "ascii"), data])), 8 + data.length);
  return out;
}

function encodePng(rgba, w, h) {
  // filter 0 per scanline
  const raw = Buffer.alloc((w * 4 + 1) * h);
  for (let y = 0; y < h; y++) {
    raw[y * (w * 4 + 1)] = 0;
    rgba.copy(raw, y * (w * 4 + 1) + 1, y * w * 4, (y + 1) * w * 4);
  }
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0);
  ihdr.writeUInt32BE(h, 4);
  ihdr[8] = 8; ihdr[9] = 6; ihdr[10] = 0; ihdr[11] = 0; ihdr[12] = 0; // 8-bit RGBA
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", ihdr),
    chunk("IDAT", deflateSync(raw, { level: 9 })),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

// Brand mark: dark slate rounded tile, amber anvil silhouette, white spark.
function drawIcon(size) {
  const px = Buffer.alloc(size * size * 4);
  const s = size;
  const set = (x, y, r, g, b, a = 255) => {
    if (x < 0 || y < 0 || x >= s || y >= s) return;
    const i = (y * s + x) * 4;
    px[i] = r; px[i + 1] = g; px[i + 2] = b; px[i + 3] = a;
  };
  const bg = [30, 34, 44];
  const edge = 0.18 * s; // rounded-corner radius
  for (let y = 0; y < s; y++) {
    for (let x = 0; x < s; x++) {
      // rounded-rect coverage via corner distance
      const cx = Math.min(x, s - 1 - x), cy = Math.min(y, s - 1 - y);
      const inside = cx >= 0 && cy >= 0 && (cx + cy >= edge * 0.35 || (cx > edge && cy > edge));
      if (inside) set(x, y, bg[0], bg[1], bg[2]);
    }
  }
  const fg = [235, 238, 245];
  const amber = [255, 176, 32];
  const f = s / 64; // design in a 64-grid
  const R = (x0, y0, x1, y1, col = fg) => {
    for (let y = Math.round(y0 * f); y < Math.round(y1 * f); y++)
      for (let x = Math.round(x0 * f); x < Math.round(x1 * f); x++)
        set(x, y, col[0], col[1], col[2]);
  };
  // anvil: top beak bar, waist, base
  R(14, 26, 50, 34);
  R(20, 34, 34, 40);
  R(40, 26, 50, 30, amber); // hot bar on the right of the face
  R(24, 40, 40, 44);
  R(18, 44, 46, 50);
  // sparks
  const sparks = [[52, 18], [56, 22], [48, 14]];
  for (const [sx, sy] of sparks) R(sx - 1, sy - 1, sx + 1, sy + 1, amber);
  return { px, size: s };
}

function writeP(name, size) {
  const { px, size: s } = drawIcon(size);
  const file = join(root, name);
  mkdirSync(dirname(file), { recursive: true });
  writeFileSync(file, encodePng(px, s, s));
  console.log("wrote", file, s + "px", "bytes");
}

writeP("32x32.png", 32);
writeP("128x128.png", 128);
writeP("128x128@2x.png", 256);
writeP("icon.png", 512);

// .ico wrapping the 32px PNG (Vista+ PNG-compressed icon entry).
const png32 = (() => {
  const { px, size } = drawIcon(32);
  return encodePng(px, size, size);
})();
const entry = Buffer.alloc(16);
entry[0] = 32; entry[1] = 32; // width, height (0 means 256)
entry[2] = 0; entry[3] = 0;   // colors, reserved
entry.writeUInt16LE(1, 4);    // planes
entry.writeUInt16LE(32, 6);   // bpp
entry.writeUInt32LE(png32.length, 8);
entry.writeUInt32LE(6 + 16, 12); // offset
const ico = Buffer.concat([
  Buffer.from([0, 0, 1, 0, 1, 0]), // ICONDIR: 1 image
  entry,
  png32,
]);
writeFileSync(join(root, "icon.ico"), ico);
console.log("wrote", join(root, "icon.ico"), ico.length, "bytes");
