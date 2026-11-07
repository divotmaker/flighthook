'use strict';
// Writes a stance clip: an MJPEG AVI 1.0 file (one video stream, a `00dc`
// chunk per JPEG frame, an `idx1` index) and its JSON sidecar, the format
// stance's recorder writes and its `AviFile` plays.

const fs = require('node:fs');

const HEADER_LEN = 224;
const MOVI_FOURCC = 220;

function u32(v) {
  const b = Buffer.alloc(4);
  b.writeUInt32LE(v >>> 0);
  return b;
}

function u16(v) {
  const b = Buffer.alloc(2);
  b.writeUInt16LE(v & 0xffff);
  return b;
}

function header({ width, height, fpsNum, fpsDen, frames, maxFrame, riffSize, moviSize }) {
  const microsPerFrame = Math.floor((fpsDen * 1e6) / fpsNum);
  const maxBps = Math.floor((maxFrame * fpsNum) / fpsDen);
  const h = Buffer.concat([
    Buffer.from('RIFF'), u32(riffSize), Buffer.from('AVI '),
    Buffer.from('LIST'), u32(192), Buffer.from('hdrl'),
    Buffer.from('avih'), u32(56),
    u32(microsPerFrame), u32(maxBps), u32(0), u32(0x10), u32(frames), u32(0), u32(1),
    u32(maxFrame + 8), u32(width), u32(height), Buffer.alloc(16),
    Buffer.from('LIST'), u32(116), Buffer.from('strl'),
    Buffer.from('strh'), u32(56), Buffer.from('vids'), Buffer.from('MJPG'),
    u32(0), u16(0), u16(0), u32(0), u32(fpsDen), u32(fpsNum), u32(0), u32(frames),
    u32(maxFrame + 8), u32(0xffffffff), u32(0), u16(0), u16(0), u16(width), u16(height),
    Buffer.from('strf'), u32(40), u32(40), u32(width), u32(height), u16(1), u16(24),
    Buffer.from('MJPG'), u32(width * height * 3), Buffer.alloc(16),
    Buffer.from('LIST'), u32(moviSize), Buffer.from('movi'),
  ]);
  if (h.length !== HEADER_LEN) throw new Error(`AVI header is ${h.length} bytes`);
  return h;
}

/**
 * Write `<stem>.avi` and `<stem>.json`.
 * @param {string} stem      path without extension
 * @param {Buffer[]} jpegs   the frames
 * @param {object} o
 * @param {number} o.width
 * @param {number} o.height
 * @param {number} o.fps       nominal frame rate (an integer)
 * @param {number[]} o.tNs     each frame's time after the first, ns
 * @param {number} o.impactNs  the labelled impact, ns after the first frame
 */
function writeClip(stem, jpegs, { width, height, fps, tNs, impactNs }) {
  const body = [];
  const index = [];
  const frames = [];
  let pos = HEADER_LEN;
  let maxFrame = 0;
  jpegs.forEach((jpeg, i) => {
    const pad = jpeg.length & 1;
    index.push(Buffer.concat([Buffer.from('00dc'), u32(0x10), u32(pos - MOVI_FOURCC), u32(jpeg.length)]));
    frames.push({ t_ns: tNs[i], offset: pos + 8, len: jpeg.length });
    body.push(Buffer.from('00dc'), u32(jpeg.length), jpeg, Buffer.alloc(pad));
    pos += 8 + jpeg.length + pad;
    maxFrame = Math.max(maxFrame, jpeg.length);
  });
  const idx = Buffer.concat([Buffer.from('idx1'), u32(16 * index.length), ...index]);
  const head = header({
    width, height, fpsNum: fps, fpsDen: 1, frames: jpegs.length, maxFrame,
    riffSize: pos + idx.length - 8, moviSize: pos - MOVI_FOURCC,
  });
  fs.writeFileSync(`${stem}.avi`, Buffer.concat([head, ...body, idx]));
  const impactIndex = tNs.findIndex((t) => t >= impactNs);
  const sidecar = {
    version: 1,
    fourcc: 'MJPG',
    width,
    height,
    fps: { nominal: fps, measured: fps },
    start_ns: 1000000000,
    frames,
    trigger_index: impactIndex,
    impact_index: impactIndex,
    trigger_ns: impactNs,
    impact_ns: impactNs,
    complete: true,
    dropped: 0,
    truncated: false,
  };
  fs.writeFileSync(`${stem}.json`, `${JSON.stringify(sidecar, null, 2)}\n`);
}

module.exports = { writeClip };
