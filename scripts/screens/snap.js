'use strict';
// The browser half of a scene: open flighthook's web UI in Playwright
// Chromium, run the scene's UI steps and write its screenshots.
//
//   node snap.js <spec.json>
//
// run.js writes the spec and starts this script with $SCREENS_NODE (default
// `node`), so it can run wherever Playwright's browsers are installed.
//
// Spec: { url, viewport: { width, height }, quietMs, images: [{ out, steps, crop }] }
// Steps (canvas coordinates in CSS px):
//   { click: [x, y] }        click there
//   { move: [x, y] }         move the pointer there
//   { settle: true }         wait until the picture stops changing
//   { waitPixel: { box: [x0, y0, x1, y1], rgb: [r, g, b], tol, stableMs } }
//                            wait until the box shows that color in the same
//                            place for stableMs (the page may be animating)
//   { clickPixel: { box: [x0, y0, x1, y1], rgb: [r, g, b], tol } }
//                            click the middle of the first run of pixels of
//                            that color, scanning the box row by row
// Every image ends with a settle (quiet for the spec's quietMs, default
// 1.2 s); `crop: { card: [x, y], margin }` keeps only the card around (x, y).

const fs = require('node:fs');
const { chromium } = require('playwright');
const png = require('./lib/png');

const spec = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const QUIET_MS = 1200;
const EVERY_MS = 150;
const TIMEOUT_MS = 60000;

function near(a, b, tol) {
  return a.every((v, i) => Math.abs(v - b[i]) <= tol);
}

async function shoot(page, clip) {
  return page.screenshot(clip ? { clip } : {});
}

/** Screenshot until the picture is unchanged for `quietMs`; returns that picture. */
async function settle(page, clip, quietMs = QUIET_MS) {
  const end = Date.now() + TIMEOUT_MS;
  let last = await shoot(page, clip);
  let since = Date.now();
  for (;;) {
    await page.waitForTimeout(EVERY_MS);
    const next = await shoot(page, clip);
    if (!next.equals(last)) {
      last = next;
      since = Date.now();
    } else if (Date.now() - since >= quietMs) {
      return last;
    }
    if (Date.now() > end) throw new Error('the picture never settled');
  }
}

async function waitPixel(page, { box, rgb, tol = 8, stableMs = 0 }) {
  const end = Date.now() + TIMEOUT_MS;
  let seen = null;
  let since = 0;
  for (;;) {
    const at = findRun(png.decode(await shoot(page)), box, rgb, tol);
    const key = at && at.join(',');
    if (key !== seen) {
      seen = key;
      since = Date.now();
    }
    if (at && Date.now() - since >= stableMs) return;
    if (Date.now() > end) throw new Error(`no steady ${rgb} pixel in ${box}`);
    await page.waitForTimeout(EVERY_MS);
  }
}

function findRun(img, [x0, y0, x1, y1], rgb, tol) {
  for (let y = y0; y <= y1; y++) {
    for (let x = x0; x <= x1; x++) {
      if (!near(png.pixel(img, x, y), rgb, tol)) continue;
      let end = x;
      while (end + 1 <= x1 && near(png.pixel(img, end + 1, y), rgb, tol)) end += 1;
      return [(x + end) / 2, y];
    }
  }
  return null;
}

/** The card around (x, y): grow outwards while the pixels differ from the page. */
function cardBox(img, [px, py], margin) {
  const bg = png.pixel(img, 4, img.height - 1);
  const isBg = (cx, cy) => near(png.pixel(img, cx, cy), bg, 1);
  let left = px;
  while (left > 0 && !isBg(left - 1, py)) left -= 1;
  let right = px;
  while (right < img.width - 1 && !isBg(right + 1, py)) right += 1;
  let top = py;
  while (top > 0 && !isBg(px, top - 1)) top -= 1;
  let bottom = py;
  while (bottom < img.height - 1 && !isBg(px, bottom + 1)) bottom += 1;
  return {
    x: left - margin,
    y: top - margin,
    width: right - left + 1 + 2 * margin,
    height: bottom - top + 1 + 2 * margin,
  };
}

/**
 * Runs in the page before the UI, to make the order of events fixed:
 * - fetches run one at a time, in the order the UI asks, each answered with
 *   its whole body read, so replies arrive in that order too (the UI takes
 *   some things from whichever reply comes first);
 * - a frame runs only once no fetch is in flight and the network (fetches and
 *   WebSocket messages) has been quiet for FRAME_QUIET_MS;
 * - the page's clock (`performance.now`, which the UI's animations and video
 *   playback run on) moves on exactly one 60 Hz frame per frame.
 * Each frame then sees the same data and time on every run, so the UI draws
 * the same sequence of pictures. That matters beyond the final picture: egui
 * adds glyphs to its font atlas as text first appears, and the atlas layout
 * decides the last bit of the text's anti-aliasing.
 */
function frameGate() {
  const FRAME_QUIET_MS = 100;
  const FRAME_MS = 1000 / 60;
  const realNow = performance.now.bind(performance);
  let clock = 0;
  performance.now = () => clock;
  let inflight = 0;
  let lastNet = realNow();
  const done = () => {
    inflight -= 1;
    lastNet = realNow();
  };
  const track = (promise) => {
    inflight += 1;
    return promise.finally(done);
  };
  const fetch = window.fetch.bind(window);
  let queue = Promise.resolve();
  window.fetch = (...args) => {
    const reply = queue.then(async () => {
      const res = await fetch(...args);
      const body = [101, 204, 205, 304].includes(res.status) ? null : await res.arrayBuffer();
      const whole = new Response(body, { status: res.status, statusText: res.statusText, headers: res.headers });
      Object.defineProperty(whole, 'url', { value: res.url });
      return whole;
    });
    queue = track(reply).catch(() => {});
    return reply;
  };
  for (const method of ['arrayBuffer', 'blob', 'json', 'text']) {
    const read = Response.prototype[method];
    Response.prototype[method] = function body(...args) {
      return track(read.apply(this, args));
    };
  }
  const RealWebSocket = window.WebSocket;
  window.WebSocket = class extends RealWebSocket {
    constructor(...args) {
      super(...args);
      for (const event of ['open', 'message', 'close']) {
        this.addEventListener(event, () => {
          lastNet = realNow();
        });
      }
    }
  };
  const raf = window.requestAnimationFrame.bind(window);
  const cancel = window.cancelAnimationFrame.bind(window);
  const waiting = new Map();
  let next = 1;
  window.requestAnimationFrame = (callback) => {
    const id = next++;
    const attempt = () => {
      if (!waiting.has(id)) return;
      if (inflight === 0 && realNow() - lastNet >= FRAME_QUIET_MS) {
        waiting.set(id, { frame: raf(() => {
          waiting.delete(id);
          clock += FRAME_MS;
          callback(clock);
        }) });
      } else {
        waiting.set(id, { timer: setTimeout(attempt, 10) });
      }
    };
    waiting.set(id, {});
    attempt();
    return id;
  };
  window.cancelAnimationFrame = (id) => {
    const w = waiting.get(id);
    waiting.delete(id);
    if (w && w.frame) cancel(w.frame);
    if (w && w.timer) clearTimeout(w.timer);
  };
}

async function main() {
  const { width, height } = spec.viewport;
  const browser = await chromium.launch({
    headless: false,
    args: ['--headless=new', `--window-size=${width},${height + 87}`, '--use-angle=swiftshader',
      '--enable-unsafe-swiftshader', '--ignore-gpu-blocklist', '--hide-scrollbars'],
  });
  try {
    const context = await browser.newContext({
      viewport: { width, height },
      timezoneId: 'UTC',
      locale: 'en-US',
    });
    await context.addInitScript(frameGate);
    const page = await context.newPage();
    await page.goto(spec.url, { waitUntil: 'networkidle' });
    await settle(page);
    for (const image of spec.images) {
      for (const step of image.steps) {
        if (step.click) await page.mouse.click(...step.click);
        else if (step.move) await page.mouse.move(...step.move);
        else if (step.settle) await settle(page);
        else if (step.waitPixel) await waitPixel(page, step.waitPixel);
        else if (step.clickPixel) {
          const { box, rgb, tol = 8 } = step.clickPixel;
          const img = png.decode(await shoot(page));
          const at = findRun(img, box, rgb, tol);
          if (!at) throw new Error(`no ${rgb} pixel in ${box}`);
          console.log(`clickPixel ${rgb} at ${at}`);
          await page.mouse.click(...at);
        } else throw new Error(`unknown step ${JSON.stringify(step)}`);
      }
      let clip = null;
      if (image.crop) {
        clip = cardBox(png.decode(await settle(page)), image.crop.card, image.crop.margin || 0);
        console.log(`crop ${JSON.stringify(clip)}`);
      }
      fs.writeFileSync(image.out, await settle(page, clip, spec.quietMs || QUIET_MS));
      console.log(`saved ${image.out}`);
    }
  } finally {
    await browser.close();
  }
}

main().catch((e) => {
  console.error(e.stack || e);
  process.exit(1);
});
