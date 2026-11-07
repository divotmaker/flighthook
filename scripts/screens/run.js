#!/usr/bin/env node
'use strict';
// Regenerates the README screenshots in screens/ (see docs/SCREENSHOTS.md).
//
//   node scripts/screens/run.js [--only a,b] [--check] [--out DIR] [--keep] [--no-build]
//
// --only   scenes (or image names) to run, comma-separated; default all
// --check  write into target/screenshots/check and compare with screens/: exit 1
//          and list the files whose picture differs
// --out    write the images here instead of screens/
// --keep   keep each scene's scratch folder (config, data, log)
// --no-build  use the flighthook binary already built
//
// Each scene (scenes/*.js) starts its own headless flighthook on localhost
// with a scratch config and data folder, drives it to the scene's state,
// screenshots the web UI with Playwright Chromium and stops everything it
// started. Scenes run in parallel after one build.
//
// Environment: FFMPEG (default `ffmpeg`), SCREENS_NODE (the command that runs
// the Playwright script, default `node`), CARGO_TARGET_DIR as for cargo.

const fs = require('node:fs');
const path = require('node:path');
const util = require('./lib/util');
const { Flighthook } = require('./lib/flighthook');
const footage = require('./lib/footage');
const png = require('./lib/png');

const ROOT = path.resolve(__dirname, '..', '..');
const WORK = path.join(ROOT, 'target', 'screenshots');
const CACHE = path.join(WORK, 'cache');
const SCREENS = path.join(ROOT, 'screens');

function parseArgs(argv) {
  const args = { only: null, check: false, out: null, keep: false, build: true };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--only') args.only = argv[++i].split(',').map((s) => s.trim()).filter(Boolean);
    else if (a === '--check') args.check = true;
    else if (a === '--out') args.out = path.resolve(argv[++i]);
    else if (a === '--keep') args.keep = true;
    else if (a === '--no-build') args.build = false;
    else throw new Error(`unknown argument ${a} (see the top of ${__filename})`);
  }
  return args;
}

function loadScenes(only) {
  const dir = path.join(__dirname, 'scenes');
  const scenes = fs.readdirSync(dir).filter((f) => f.endsWith('.js')).sort()
    .map((f) => require(path.join(dir, f)));
  if (!only) return scenes;
  const picked = scenes.filter((s) => only.includes(s.name)
    || s.snap({}).images.some((i) => only.includes(i.name)));
  if (!picked.length) throw new Error(`no scene matches ${only.join(',')}`);
  return picked;
}

/**
 * Build the web UI into a staging folder and copy into ui/dist only the files
 * that changed: flighthook embeds ui/dist, so an untouched file leaves cargo
 * nothing to rebuild.
 */
async function buildUi(out) {
  const staging = path.join(WORK, 'dist');
  await util.run('trunk', ['build', '--release', '--dist', staging], { cwd: path.join(ROOT, 'ui'), log: out });
  const dist = path.join(ROOT, 'ui', 'dist');
  fs.mkdirSync(dist, { recursive: true });
  for (const name of fs.readdirSync(staging)) {
    const from = path.join(staging, name);
    const to = path.join(dist, name);
    if (!fs.statSync(from).isFile()) continue;
    if (!fs.existsSync(to) || !fs.readFileSync(to).equals(fs.readFileSync(from))) fs.copyFileSync(from, to);
  }
}

async function build(log) {
  const out = fs.createWriteStream(path.join(WORK, 'build.log'));
  log('building the web UI (trunk) and flighthook (screens profile)');
  await buildUi(out);
  await util.run('cargo', ['build', '--locked', '--profile', 'screens', '-p', 'flighthook-app',
    '--no-default-features', '--features', 'headless'], { cwd: ROOT, log: out });
  out.end();
}

async function binary() {
  const meta = JSON.parse(await util.run('cargo', ['metadata', '--no-deps', '--format-version', '1'], { cwd: ROOT }));
  const bin = path.join(meta.target_directory, 'screens', 'flighthook');
  if (!fs.existsSync(bin)) throw new Error(`${bin} is missing: run without --no-build`);
  return bin;
}

/** A scene that cannot run here; reported, not a failure. */
class Skip extends Error {}

/** Ports for a scene: fixed ones (shown on screen) must be free; 'free' ones are picked. */
const handedOut = new Set();
async function ports(scene, fixed) {
  const out = {};
  for (const [key, want] of Object.entries(scene.ports)) {
    if (want === 'free') {
      let port;
      do port = await util.freePort(); while (handedOut.has(port) || fixed.has(port));
      handedOut.add(port);
      out[key] = port;
    } else {
      if (!(await util.portFree(want))) {
        throw new Error(`port ${want} (${key}) is in use; the scene shows it, so it needs that port`);
      }
      out[key] = want;
    }
  }
  return out;
}

/** Run snap.js with $SCREENS_NODE (default `node`). */
function snap(specFile, log) {
  const node = process.env.SCREENS_NODE || 'node';
  return util.run('/bin/sh', ['-c', `${node} "$@"`, 'sh', path.join(__dirname, 'snap.js'), specFile], { log });
}

async function runScene(scene, { bin, outDir, keep, fixed, footageReady }) {
  const timer = new util.Timer();
  const log = (m) => console.log(`[${scene.name}] ${m}`);
  const dir = path.join(WORK, 'run', scene.name);
  util.rmrf(dir);
  fs.mkdirSync(dir, { recursive: true });
  const ctx = { scene, dir, log, timer, cleanup: [], models: path.join(CACHE, 'models') };
  let ok = false;
  let skipped = false;
  try {
    ctx.ports = await ports(scene, fixed);
    if ((scene.needs || []).includes('footage')) ctx.footage = await timer.stage('footage', () => footageReady);
    ctx.fh = new Flighthook({ bin, dir, config: scene.config(ctx), port: ctx.ports.web, env: scene.env });
    if (scene.before) await scene.before(ctx);
    await timer.stage('start', () => ctx.fh.start());
    if ((scene.needs || []).includes('cameras') && !(await ctx.fh.get('/api/status')).cameras_supported) {
      throw new Skip('flighthook has no camera support on this platform, and the scene shows cameras');
    }
    await timer.stage('feed', () => scene.drive(ctx));
    const spec = scene.snap(ctx);
    const images = spec.images.map((i) => ({ ...i, out: path.join(dir, `${i.name}.png`) }));
    const specFile = path.join(dir, 'snap.json');
    fs.writeFileSync(specFile, JSON.stringify({
      ...spec, images, url: `http://127.0.0.1:${ctx.ports.web}/`, quietMs: scene.quietMs,
    }, null, 2));
    const snapLog = fs.createWriteStream(path.join(dir, 'snap.log'));
    await timer.stage('capture', () => snap(specFile, snapLog));
    snapLog.end();
    if (scene.verify) await scene.verify(ctx);
    // An image that shows the same picture as the one already there is left
    // as it is, so git sees only the screenshots whose picture changed.
    for (const i of images) {
      const to = path.join(outDir, `${i.name}.png`);
      if (!fs.existsSync(to) || !png.same(fs.readFileSync(to), fs.readFileSync(i.out))) fs.copyFileSync(i.out, to);
    }
    ok = true;
    log(`done in ${(timer.total() / 1000).toFixed(1)} s: ${images.map((i) => `${i.name}.png`).join(', ')}`);
    return { name: scene.name, images: images.map((i) => i.name), stages: timer.stages, total: timer.total() };
  } catch (e) {
    if (!(e instanceof Skip)) throw e;
    skipped = true;
    log(`skipped: ${e.message}`);
    return { name: scene.name, images: [], skipped: e.message, stages: timer.stages, total: timer.total() };
  } finally {
    if (ctx.fh) await ctx.fh.stop();
    for (const close of ctx.cleanup.reverse()) {
      try {
        await close();
      } catch {
        // already closed
      }
    }
    if ((ok && !keep) || skipped) util.rmrf(dir);
    else if (!ok) log(`failed; scratch folder kept: ${dir}`);
  }
}

function table(results, buildMs, totalMs) {
  const s = (ms) => (ms === undefined ? '-' : (ms / 1000).toFixed(1));
  console.log('\nscene                   footage  start  models   feed  capture  total (s)');
  for (const r of results) {
    if (r.skipped) {
      console.log(`${r.name.padEnd(22)} skipped`);
      continue;
    }
    const st = r.stages;
    const feed = st.feed === undefined ? undefined : st.feed - (st.models || 0);
    console.log(`${r.name.padEnd(22)} ${s(st.footage).padStart(8)} ${s(st.start).padStart(6)} `
      + `${s(st.models).padStart(7)} ${s(feed).padStart(6)} ${s(st.capture).padStart(8)} ${s(r.total).padStart(6)}`);
  }
  console.log(`build ${s(buildMs)} s, all ${s(totalMs)} s\n`);
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const t0 = Date.now();
  const log = (m) => console.log(`[screens] ${m}`);
  fs.mkdirSync(CACHE, { recursive: true });
  const scenes = loadScenes(args.only);
  const outDir = args.check ? path.join(WORK, 'check') : args.out || SCREENS;
  if (args.check) util.rmrf(outDir);
  fs.mkdirSync(outDir, { recursive: true });

  // The footage is fetched and cut while flighthook builds.
  const footageReady = scenes.some((s) => (s.needs || []).includes('footage'))
    ? footage.ensureClips(CACHE, { ffmpeg: process.env.FFMPEG || 'ffmpeg', log })
    : Promise.resolve(null);
  footageReady.catch(() => {});
  const tb = Date.now();
  if (args.build) await build(log);
  const buildMs = args.build ? Date.now() - tb : undefined;
  const bin = await binary();

  const fixed = new Set(scenes.flatMap((s) => Object.values(s.ports).filter((p) => p !== 'free')));
  const settled = await Promise.allSettled(scenes.map((scene) =>
    runScene(scene, { bin, outDir, keep: args.keep, fixed, footageReady })));
  const results = settled.filter((r) => r.status === 'fulfilled').map((r) => r.value);
  const failed = settled.filter((r) => r.status === 'rejected');
  const totalMs = Date.now() - t0;
  table(results, buildMs, totalMs);
  fs.writeFileSync(path.join(WORK, 'timings.json'), JSON.stringify({ buildMs, totalMs, results }, null, 2));
  for (const f of failed) console.error(`error: ${f.reason.stack || f.reason}`);
  if (failed.length) process.exit(1);

  if (args.check) {
    const differ = results.flatMap((r) => r.images).filter((name) => {
      const committed = path.join(SCREENS, `${name}.png`);
      return !fs.existsSync(committed)
        || !png.same(fs.readFileSync(committed), fs.readFileSync(path.join(outDir, `${name}.png`)));
    });
    if (differ.length) {
      console.error(`screens differ from ${path.relative(ROOT, outDir)}/ (regenerate with make screens):`);
      for (const name of differ) console.error(`  screens/${name}.png`);
      process.exit(1);
    }
    log('screens/ is up to date');
  }
}

main().catch((e) => {
  console.error(`error: ${e.stack || e}`);
  process.exit(1);
});
