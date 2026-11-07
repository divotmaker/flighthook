'use strict';
// Swing footage for the Cameras scene: a CC0 slow-motion video from
// Wikimedia Commons, downloaded once, checked against a pinned SHA-256, then
// cut and cropped into two stance clips (face-on and down-the-line) with
// ffmpeg. Everything is cached, so later runs skip it.

const crypto = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');
const { run, rmrf } = require('./util');
const { writeClip } = require('./avi');

/** The source: a Wikimedia Commons file, CC0 1.0, found by its page id. */
const SOURCE = {
  pageId: 197476291,
  sha256: '7b748b86b794790bd079b629fb124b71e446d228aeabf12ddfe11b46a1406c44',
  bytes: 17891489,
  fps: 30,
  // Slow motion, played back 8x slower than it was filmed (assumed): the
  // clips' times are playback time / 8.
  slowdown: 8,
};

/**
 * The clips: source frames [from, to), the source frame of impact (x.5 =
 * between two frames), and the crop (w, h, x, y) of the 1080x1920 picture.
 */
const CLIPS = {
  'golfer-front': { from: 1540, to: 2000, impact: 1839.5, crop: [1080, 900, 0, 435] },
  'target-line': { from: 0, to: 430, impact: 278, crop: [960, 800, 60, 480] },
};

/** JPEG quality for ffmpeg's MJPEG encoder (2 = best, 31 = worst). */
const QUALITY = 2;

const USER_AGENT = 'flighthook-screens/1 (https://github.com/divotmaker/flighthook)';

function sha256File(file) {
  return crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
}

async function download(dest, log) {
  if (fs.existsSync(dest) && sha256File(dest) === SOURCE.sha256) return;
  log(`footage: downloading Wikimedia Commons file (page ${SOURCE.pageId}, ${(SOURCE.bytes / 1e6).toFixed(1)} MB)`);
  const api = 'https://commons.wikimedia.org/w/api.php?action=query&prop=imageinfo&iiprop=url'
    + `&format=json&pageids=${SOURCE.pageId}`;
  const info = await (await fetch(api, { headers: { 'user-agent': USER_AGENT } })).json();
  const url = info.query.pages[SOURCE.pageId].imageinfo[0].url;
  const res = await fetch(url, { headers: { 'user-agent': USER_AGENT } });
  if (!res.ok) throw new Error(`footage download failed: HTTP ${res.status}`);
  const data = Buffer.from(await res.arrayBuffer());
  const sum = crypto.createHash('sha256').update(data).digest('hex');
  if (sum !== SOURCE.sha256) throw new Error(`footage SHA-256 mismatch: got ${sum}, want ${SOURCE.sha256}`);
  fs.mkdirSync(path.dirname(dest), { recursive: true });
  fs.writeFileSync(`${dest}.part`, data);
  fs.renameSync(`${dest}.part`, dest);
}

/**
 * The two clips, built if the cache lacks them. Returns `{ name: avi path }`.
 * @param {string} cacheDir
 * @param {object} o
 * @param {string} o.ffmpeg  the ffmpeg command
 * @param {function} o.log
 */
async function ensureClips(cacheDir, { ffmpeg, log }) {
  const version = (await run(ffmpeg, ['-hide_banner', '-version'])).split('\n')[0].trim();
  const recipe = JSON.stringify({ SOURCE, CLIPS, QUALITY, version, format: 1 });
  const key = crypto.createHash('sha256').update(recipe).digest('hex').slice(0, 16);
  const dir = path.join(cacheDir, 'footage', key);
  const out = Object.fromEntries(Object.keys(CLIPS).map((n) => [n, path.join(dir, `${n}.avi`)]));
  if (fs.existsSync(path.join(dir, 'recipe.json'))) return out;

  const source = path.join(cacheDir, 'downloads', `${SOURCE.sha256}.webm`);
  await download(source, log);
  log(`footage: cutting the clips with ${version}`);
  const tmp = `${dir}.tmp`;
  rmrf(tmp);
  const names = Object.keys(CLIPS);
  const split = `[0:v]split=${names.length}${names.map((_, i) => `[s${i}]`).join('')}`;
  const chains = names.map((name, i) => {
    const { from, to, crop: [w, h, x, y] } = CLIPS[name];
    return `[s${i}]select='between(n\\,${from}\\,${to - 1})',crop=${w}:${h}:${x}:${y}[o${i}]`;
  });
  const args = ['-nostdin', '-hide_banner', '-loglevel', 'error', '-i', source,
    '-filter_complex', [split, ...chains].join(';')];
  names.forEach((name, i) => {
    fs.mkdirSync(path.join(tmp, name), { recursive: true });
    const { from, to } = CLIPS[name];
    args.push('-map', `[o${i}]`, '-fps_mode', 'passthrough', '-frames:v', String(to - from),
      '-c:v', 'mjpeg', '-q:v', String(QUALITY), '-pix_fmt', 'yuvj420p', '-f', 'image2',
      path.join(tmp, name, '%06d.jpg'));
  });
  await run(ffmpeg, args);

  const rate = SOURCE.fps * SOURCE.slowdown;
  for (const name of names) {
    const { from, to, impact, crop: [width, height] } = CLIPS[name];
    const files = fs.readdirSync(path.join(tmp, name)).sort();
    if (files.length !== to - from) throw new Error(`${name}: ffmpeg wrote ${files.length} frames, want ${to - from}`);
    const jpegs = files.map((f) => fs.readFileSync(path.join(tmp, name, f)));
    const tNs = jpegs.map((_, i) => Math.round((i * 1e9) / rate));
    const impactNs = Math.round(((impact - from) * 1e9) / rate);
    writeClip(path.join(tmp, name), jpegs, { width, height, fps: rate, tNs, impactNs });
    rmrf(path.join(tmp, name));
  }
  fs.writeFileSync(path.join(tmp, 'recipe.json'), `${recipe}\n`);
  rmrf(dir);
  fs.renameSync(tmp, dir);
  return out;
}

module.exports = { ensureClips, SOURCE };
