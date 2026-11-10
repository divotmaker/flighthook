'use strict';
// screens/devices.png and screens/devices-settings.png: the Devices tab with
// a Mock and an Open Connect launch monitor (a Uneekor), a test-pattern
// camera and GSPro; then the same with the camera card on its Settings side,
// its Rotation in sight under the camera.
//
// The cards show ports, so this scene uses fixed ones. The Uneekor sends six
// 7-iron shots; the Mock's first shot comes 33 s after it starts, so the
// scene is done before then (shot_count 0). The Web Server card counts HTTP
// requests, so the scene makes exactly one before the browser's (a status
// check) and settles across a telemetry update (every 5 s).

const { sleep } = require('../lib/util');
const { Monitor, Simulator } = require('../lib/openconnect');

const MOCK_FIRST_SHOT_MS = 33000;

function shot(n) {
  const v = Math.sin(n * 1.3);
  const w = Math.cos(n * 0.9);
  const r1 = (x) => Math.round(x * 10) / 10;
  const back = 6200 + 450 * v;
  const side = -320 + 260 * w;
  return {
    ball: {
      Speed: r1(123.0 + 2.6 * v), VLA: r1(17.4 + 1.1 * w), HLA: r1(-0.6 + 1.6 * w),
      BackSpin: Math.round(back), SideSpin: Math.round(side),
      TotalSpin: Math.round(Math.hypot(back, side)),
      SpinAxis: r1((Math.atan2(side, back) * 180) / Math.PI),
    },
    club: {
      Speed: r1(85.0 + 2.0 * v), AngleOfAttack: r1(-4.2 + 0.7 * w), FaceToTarget: r1(-0.9 + 1.5 * w),
      Path: r1(1.4 + 0.8 * v), Loft: r1(22.5 + 1.2 * v),
      HorizontalFaceImpact: r1(3.0 + 7.0 * w), VerticalFaceImpact: r1(-2.5 + 4.0 * v),
    },
  };
}

module.exports = {
  name: 'devices',
  needs: ['cameras'],
  ports: { web: 5971, monitor: 5972, gspro: 5973, peer: 5974 },
  env: { FLIGHTHOOK_SCREENS_CLOCK: '18:24:08' },
  config: ({ ports }) => `default_units = "imperial"
chipping_clubs = ["GW", "SW", "LW"]
putting_clubs = ["PT"]

[simulation]
auto_chip = false

[replay]
clip_dir = "clips"

[webserver.0]
name = "Web Server"
bind = "127.0.0.1:${ports.web}"

[mock_monitor.0]
name = "Mock"

[openconnect_server.0]
name = "Uneekor"
bind = "127.0.0.1:${ports.monitor}"

[camera.0]
name = "Golfer Back"
camera = "test-pattern"
position = "golfer_back"
mode = "auto"
rotation_deg = 90

[openconnect_client.0]
name = "GSPro"
address = "127.0.0.1:${ports.gspro}"
`,

  async before(ctx) {
    ctx.gspro = new Simulator(ctx.ports.gspro);
    ctx.cleanup.push(() => ctx.gspro.close());
    await ctx.gspro.listen();
  },

  async drive(ctx) {
    const { gspro } = ctx;
    await gspro.connected();
    // GSPro's card shows the player info only once it reports connected.
    await gspro.heartbeat();
    const monitor = new Monitor(ctx.ports.monitor, { localPort: ctx.ports.peer });
    ctx.cleanup.push(() => monitor.close());
    await monitor.connect();
    for (let n = 1; n <= 6; n++) {
      const { ball, club } = shot(n);
      await monitor.shot(n, ball, club);
    }
    await gspro.received(6);
    // The card's telemetry follows within one pass of GSPro's loop (100 ms).
    await sleep(500);
    const { actors } = await ctx.fh.get('/api/status');
    const card = actors['openconnect_client.0'];
    if (card.status !== 'connected' || card.telemetry.shots_sent !== '6' || card.telemetry.club !== '7I') {
      throw new Error(`GSPro's card is not ready: ${JSON.stringify(card)}`);
    }
  },

  quietMs: 5500,
  snap: () => ({
    viewport: { width: 1600, height: 1700 },
    images: [
      { name: 'devices', steps: [{ click: [55, 27] }, { move: [1590, 1690] }] },
      { name: 'devices-settings', steps: [{ click: [478, 1096] }, { move: [1590, 1690] }] },
    ],
  }),

  async verify(ctx) {
    const { actors } = await ctx.fh.get('/api/status');
    const mock = actors['mock_monitor.0'];
    if (!mock || mock.telemetry.shot_count !== '0') {
      throw new Error(`the Mock shot before the screenshots were done (over ${MOCK_FIRST_SHOT_MS / 1000} s)`);
    }
  },
};
