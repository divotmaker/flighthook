'use strict';
// Just enough PNG for the scenes: decode the browser's 8-bit RGB/RGBA
// screenshots to pixels (to find things on the canvas), and compare them.

const zlib = require('node:zlib');

const SIGNATURE = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);

function chunks(buf) {
  if (!buf.subarray(0, 8).equals(SIGNATURE)) throw new Error('not a PNG');
  const out = [];
  for (let at = 8; at < buf.length;) {
    const len = buf.readUInt32BE(at);
    out.push({ type: buf.toString('latin1', at + 4, at + 8), data: buf.subarray(at + 8, at + 8 + len) });
    at += 12 + len;
  }
  return out;
}

/** Decode to `{ width, height, channels, data }` (rows of 8-bit samples). */
function decode(buf) {
  const parts = chunks(buf);
  const ihdr = parts.find((c) => c.type === 'IHDR').data;
  const width = ihdr.readUInt32BE(0);
  const height = ihdr.readUInt32BE(4);
  const [depth, colour, , , interlace] = ihdr.subarray(8, 13);
  const channels = { 2: 3, 6: 4 }[colour];
  if (depth !== 8 || !channels || interlace) throw new Error(`unsupported PNG (depth ${depth}, colour ${colour})`);
  const raw = zlib.inflateSync(Buffer.concat(parts.filter((c) => c.type === 'IDAT').map((c) => c.data)));
  const stride = width * channels;
  const data = Buffer.alloc(stride * height);
  for (let y = 0; y < height; y++) {
    const filter = raw[y * (stride + 1)];
    const src = raw.subarray(y * (stride + 1) + 1, (y + 1) * (stride + 1));
    const row = data.subarray(y * stride, (y + 1) * stride);
    const prev = y ? data.subarray((y - 1) * stride, y * stride) : null;
    for (let x = 0; x < stride; x++) {
      const a = x >= channels ? row[x - channels] : 0;
      const b = prev ? prev[x] : 0;
      const c = prev && x >= channels ? prev[x - channels] : 0;
      let v = src[x];
      if (filter === 1) v += a;
      else if (filter === 2) v += b;
      else if (filter === 3) v += (a + b) >> 1;
      else if (filter === 4) {
        const p = a + b - c;
        const pa = Math.abs(p - a);
        const pb = Math.abs(p - b);
        const pc = Math.abs(p - c);
        v += pa <= pb && pa <= pc ? a : pb <= pc ? b : c;
      }
      row[x] = v & 0xff;
    }
  }
  return { width, height, channels, data };
}

/** The `[r, g, b]` at (x, y). */
function pixel(img, x, y) {
  const at = (y * img.width + x) * img.channels;
  return [img.data[at], img.data[at + 1], img.data[at + 2]];
}

/**
 * Whether two PNGs show the same picture. Text anti-aliasing can still move
 * a pixel by a step or two between runs (egui's font atlas), so up to
 * `pixels` pixels may differ by up to `step` in any channel.
 */
function same(a, b, { pixels = 16, step = 8 } = {}) {
  if (a.equals(b)) return true;
  const x = decode(a);
  const y = decode(b);
  if (x.width !== y.width || x.height !== y.height) return false;
  let off = 0;
  for (let i = 0, n = x.width * x.height; i < n; i++) {
    let d = 0;
    for (let c = 0; c < 3; c++) d = Math.max(d, Math.abs(x.data[i * x.channels + c] - y.data[i * y.channels + c]));
    if (d > step || (d > 0 && ++off > pixels)) return false;
  }
  return true;
}

module.exports = { chunks, decode, pixel, same };
