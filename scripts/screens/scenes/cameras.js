'use strict';
// screens/cameras.png: the Cameras tab. Two cameras play the CC0
// swing clips (face-on and down-the-line, see lib/footage.js) through
// flighthook's `clip:` camera, body tracking is Live, and two driver shots
// come from an Open Connect launch monitor. Shot #2 is shown with both clips
// paused on the impact tick, its numbers colored against the tour bands. Body tracking starts Historical (a clip is
// tracked when it is first viewed) and turns Live before shot #2, so only
// shot #2's clips are tracked: shot #1's are never shown.
//
// With FLIGHTHOOK_SCREENS_GRID_MS both clips put their impact on the same
// 2 s grid and every trigger is stamped on it, so each shot's clips line up
// with the swing's impact and hold the same frames on every run.

const fs = require('node:fs');
const path = require('node:path');
const { until } = require('../lib/util');
const { Monitor } = require('../lib/openconnect');

const BALL = { Speed: 150.0, VLA: 12.0, HLA: 1.0, TotalSpin: 2800.0, SpinAxis: 2.0 };
const CAMERAS = ['camera.0', 'camera.1'];

/** Wait until shot `n` has both cameras' clips (`analyzed`: with keypoints and swing analysis). */
async function clips(fh, n, analyzed) {
  return until(`shot ${n}'s clips${analyzed ? ', tracked and analyzed' : ''}`, async () => {
    const shots = await fh.get('/api/shots?limit=50');
    const shot = shots.find((s) => s.shot_number === n);
    const video = (shot && shot.video) || [];
    return CAMERAS.every((c) => video.some((v) => v.camera === c && (!analyzed || (v.pose && v.swing))));
  }, { timeoutMs: 180000, everyMs: 250 });
}

module.exports = {
  name: 'cameras',
  needs: ['footage', 'models', 'cameras'],
  ports: { web: 'free', monitor: 'free' },
  env: { FLIGHTHOOK_SCREENS_GRID_MS: '2000' },
  config: ({ ports, footage }) => `default_units = "imperial"
chipping_clubs = ["GW", "SW", "LW"]
putting_clubs = ["PT"]

[simulation]
auto_chip = false

[replay]
keep_shots = 20
pre_roll_s = 1.12
post_roll_s = 1.2
start_before_impact_s = 1.12
clip_dir = "clips"

[body_tracking]
mode = "historical"
golfer_height = 170

[webserver.0]
name = "Web Server"
bind = "127.0.0.1:${ports.web}"

[openconnect_server.0]
name = "Launch Monitor"
bind = "127.0.0.1:${ports.monitor}"
impact_lead_s = 0.0

[camera.0]
name = "Golfer Front"
camera = ${JSON.stringify(`clip:${footage['golfer-front']}`)}
position = "golfer_front"
mode = "auto"
latency_s = 0.0

[camera.1]
name = "Target Line"
camera = ${JSON.stringify(`clip:${footage['target-line']}`)}
position = "target_line"
mode = "auto"
latency_s = 0.0
`,

  /** The models folder is the cache's, so a model is downloaded once. */
  async before(ctx) {
    const dir = ctx.fh.modelsDir();
    fs.mkdirSync(path.dirname(dir), { recursive: true });
    fs.mkdirSync(ctx.models, { recursive: true });
    fs.symlinkSync(ctx.models, dir, 'dir');
  },

  async drive(ctx) {
    const { fh } = ctx;
    await ctx.timer.stage('models', async () => {
      let models = await fh.get('/api/body_tracking/models');
      if (models.state !== 'ready') {
        ctx.log('downloading the body tracking models (once; cached)');
        await fh.post('/api/body_tracking/models/download');
        models = await until('the body tracking models', async () => {
          const m = await fh.get('/api/body_tracking/models');
          if (m.state === 'failed') throw new Error(`model download failed: ${m.error}`);
          return m.state === 'ready' && m;
        }, { timeoutMs: 600000, everyMs: 500 });
      }
    });
    await until('both cameras', async () => {
      const { cards: actors } = await fh.get('/api/status');
      return CAMERAS.every((c) => actors[c] && actors[c].status === 'connected');
    });
    // The club picks the bands the swing numbers are graded against.
    await fh.post('/api/select', { club: 'DR' });
    const monitor = new Monitor(ctx.ports.monitor, { deviceId: 'Harness' });
    ctx.cleanup.push(() => monitor.close());
    await monitor.connect();
    await monitor.shot(1, BALL);
    await clips(fh, 1, false);
    const settings = await fh.get('/api/settings');
    settings.body_tracking.mode = 'live';
    await fh.post('/api/settings?scope=body_tracking', settings);
    await monitor.shot(2, BALL);
    await clips(fh, 2, true);
  },

  snap: () => ({
    // The app opens on Shots, whose camera cards keep playing.
    openSettle: false,
    viewport: { width: 1280, height: 944 },
    images: [{
      name: 'cameras',
      steps: [
        { click: [78, 26] }, // Cameras
        { move: [640, 120] },
        // The scrubber's gold impact line: seek there (pauses both clips),
        // again once the layout has settled, then park the pointer.
        { waitPixel: { box: [226, 625, 1262, 670], rgb: [247, 201, 72], stableMs: 1500 } },
        { clickPixel: { box: [226, 625, 1262, 670], rgb: [247, 201, 72] } },
        { move: [640, 120] },
        { settle: true },
        { clickPixel: { box: [226, 625, 1262, 670], rgb: [247, 201, 72] } },
        { move: [640, 120] },
      ],
    }],
  }),
};
