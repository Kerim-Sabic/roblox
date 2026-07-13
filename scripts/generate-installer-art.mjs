// Generates the NSIS installer artwork (header 150x57, sidebar 164x314) in
// NectarPilot's Fluent Honey palette, matching assets/brand/nectarpilot.svg:
// dark #17140f, honey gradient #ffd76a -> #e99412, cream #fff1b6.
// Output is uncompressed 24-bit bottom-up BMP, the format NSIS requires.
//
//   node scripts/generate-installer-art.mjs
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const outDir = join(root, "apps", "desktop", "src-tauri", "installer");
mkdirSync(outDir, { recursive: true });

const DARK = [0x17, 0x14, 0x0f];
const DARK_WARM = [0x24, 0x1c, 0x10];
const HONEY_LIGHT = [0xff, 0xd7, 0x6a];
const HONEY_DEEP = [0xe9, 0x94, 0x12];
const CREAM = [0xff, 0xf1, 0xb6];

const mix = (a, b, t) => a.map((v, i) => Math.round(v + (b[i] - v) * t));

class Canvas {
  constructor(width, height) {
    this.width = width;
    this.height = height;
    this.data = new Uint8Array(width * height * 3);
  }
  set(x, y, [r, g, b], alpha = 1) {
    x = Math.round(x);
    y = Math.round(y);
    if (x < 0 || y < 0 || x >= this.width || y >= this.height) return;
    const i = (y * this.width + x) * 3;
    this.data[i] = Math.round(this.data[i] + (r - this.data[i]) * alpha);
    this.data[i + 1] = Math.round(this.data[i + 1] + (g - this.data[i + 1]) * alpha);
    this.data[i + 2] = Math.round(this.data[i + 2] + (b - this.data[i + 2]) * alpha);
  }
  fill(colorAt) {
    for (let y = 0; y < this.height; y++)
      for (let x = 0; x < this.width; x++) this.set(x, y, colorAt(x, y));
  }
  disc(cx, cy, radius, color, alpha = 1) {
    for (let y = Math.floor(cy - radius); y <= cy + radius; y++)
      for (let x = Math.floor(cx - radius); x <= cx + radius; x++) {
        const d = Math.hypot(x - cx, y - cy);
        if (d <= radius) this.set(x, y, color, alpha * Math.min(1, radius - d + 1));
      }
  }
  line(x0, y0, x1, y1, thickness, color, alpha = 1) {
    const steps = Math.ceil(Math.hypot(x1 - x0, y1 - y0)) * 2;
    for (let s = 0; s <= steps; s++) {
      const t = s / steps;
      this.disc(x0 + (x1 - x0) * t, y0 + (y1 - y0) * t, thickness / 2, color, alpha);
    }
  }
  hexagon(cx, cy, radius, thickness, color, alpha = 1) {
    const points = [];
    for (let k = 0; k < 6; k++) {
      const angle = Math.PI / 6 + (k * Math.PI) / 3; // flat-top hexagon
      points.push([cx + radius * Math.cos(angle), cy + radius * Math.sin(angle)]);
    }
    for (let k = 0; k < 6; k++) {
      const [x0, y0] = points[k];
      const [x1, y1] = points[(k + 1) % 6];
      this.line(x0, y0, x1, y1, thickness, color, alpha);
    }
  }
  fillPolygon(points, color, alpha = 1) {
    const ys = points.map((p) => p[1]);
    for (let y = Math.floor(Math.min(...ys)); y <= Math.max(...ys); y++) {
      const xs = [];
      for (let k = 0; k < points.length; k++) {
        const [x0, y0] = points[k];
        const [x1, y1] = points[(k + 1) % points.length];
        if (y0 <= y !== y1 <= y) xs.push(x0 + ((y - y0) / (y1 - y0)) * (x1 - x0));
      }
      xs.sort((a, b) => a - b);
      for (let k = 0; k + 1 < xs.length; k += 2)
        for (let x = Math.ceil(xs[k]); x <= xs[k + 1]; x++) this.set(x, y, color, alpha);
    }
  }
  /** The brand mark: honey hexagon outline + four-point navigation star. */
  brandMark(cx, cy, size) {
    // Gradient-stroked hexagon: draw per-edge with interpolated color.
    for (let k = 0; k < 6; k++) {
      const a0 = Math.PI / 6 + (k * Math.PI) / 3;
      const a1 = Math.PI / 6 + ((k + 1) * Math.PI) / 3;
      const color = mix(HONEY_LIGHT, HONEY_DEEP, k / 5);
      this.line(
        cx + size * Math.cos(a0), cy + size * Math.sin(a0),
        cx + size * Math.cos(a1), cy + size * Math.sin(a1),
        Math.max(2, size * 0.16), color,
      );
    }
    const inner = size * 0.14;
    const outer = size * 0.62;
    const star = [];
    for (let k = 0; k < 8; k++) {
      const radius = k % 2 === 0 ? outer : inner;
      const angle = -Math.PI / 2 + (k * Math.PI) / 4 + Math.PI / 8;
      star.push([cx + radius * Math.cos(angle), cy + radius * Math.sin(angle)]);
    }
    this.fillPolygon(star, mix(HONEY_LIGHT, HONEY_DEEP, 0.35));
    this.disc(cx, cy, Math.max(2, size * 0.1), DARK);
    this.hexagon(cx, cy, Math.max(2, size * 0.1) + 1, 1.5, CREAM, 0.9);
  }
  toBmp() {
    const rowBytes = Math.ceil((this.width * 3) / 4) * 4;
    const imageSize = rowBytes * this.height;
    const buffer = Buffer.alloc(54 + imageSize);
    buffer.write("BM", 0);
    buffer.writeUInt32LE(54 + imageSize, 2);
    buffer.writeUInt32LE(54, 10);
    buffer.writeUInt32LE(40, 14);
    buffer.writeInt32LE(this.width, 18);
    buffer.writeInt32LE(this.height, 22);
    buffer.writeUInt16LE(1, 26);
    buffer.writeUInt16LE(24, 28);
    buffer.writeUInt32LE(imageSize, 34);
    buffer.writeInt32LE(2835, 38);
    buffer.writeInt32LE(2835, 42);
    for (let y = 0; y < this.height; y++) {
      const src = (this.height - 1 - y) * this.width * 3; // bottom-up
      for (let x = 0; x < this.width; x++) {
        const i = 54 + y * rowBytes + x * 3;
        buffer[i] = this.data[src + x * 3 + 2]; // B
        buffer[i + 1] = this.data[src + x * 3 + 1]; // G
        buffer[i + 2] = this.data[src + x * 3]; // R
      }
    }
    return buffer;
  }
}

// ---- sidebar 164x314: dark honeycomb column with the brand mark ----
const sidebar = new Canvas(164, 314);
sidebar.fill((x, y) => mix(DARK, DARK_WARM, y / 314));
for (const [hx, hy, hr, alpha] of [
  [18, 30, 26, 0.16], [70, 62, 26, 0.12], [150, 24, 30, 0.14],
  [10, 240, 30, 0.14], [96, 286, 26, 0.12], [160, 226, 24, 0.12],
]) {
  sidebar.hexagon(hx, hy, hr, 3, HONEY_DEEP, alpha);
}
sidebar.brandMark(82, 128, 46);
for (let x = 0; x < 164; x++) {
  const color = mix(HONEY_LIGHT, HONEY_DEEP, x / 164);
  for (let y = 306; y < 314; y++) sidebar.set(x, y, color);
}
writeFileSync(join(outDir, "sidebar.bmp"), sidebar.toBmp());

// ---- header 150x57: compact mark with gradient underline ----
const header = new Canvas(150, 57);
header.fill((x) => mix(DARK, DARK_WARM, x / 150));
header.hexagon(128, 10, 16, 2.5, HONEY_DEEP, 0.18);
header.hexagon(148, 44, 14, 2.5, HONEY_DEEP, 0.14);
header.brandMark(28, 26, 17);
for (let x = 0; x < 150; x++) {
  const color = mix(HONEY_LIGHT, HONEY_DEEP, x / 150);
  for (let y = 52; y < 57; y++) header.set(x, y, color);
}
writeFileSync(join(outDir, "header.bmp"), header.toBmp());

console.log("wrote installer art:", outDir);
