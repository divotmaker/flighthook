'use strict';
// screens/shots.png: the Shots tab, Cards view, Data pane, after a 7-iron
// session of 12 shots from one Open Connect launch monitor (a Uneekor),
// with GSPro connected and a camera recording (no video on the shot cards).

const { until } = require('../lib/util');
const { Monitor, Simulator } = require('../lib/openconnect');

// n: ball speed mph, VLA, HLA, backspin rpm, sidespin rpm (- = draw),
//    face impact toe(+)/heel(-) mm, high(+)/low(-) mm, attack angle
const SHOTS = [
  [124.0, 17.4, 1.4, 6300, 480, -12.0, 1.5, -4.2],
  [115.0, 14.6, -0.8, 5300, -200, 9.0, -14.0, -1.0],
  [126.5, 16.6, -1.6, 6400, -450, 16.0, 2.0, -4.6],
  [115.0, 18.8, 0.6, 7100, 150, -3.0, -8.0, -7.0],
  [122.0, 19.6, 1.0, 6900, 300, -7.0, 4.0, -4.0],
  [128.5, 15.4, -0.5, 5500, -150, 4.0, 7.0, -3.6],
  [121.0, 18.4, 1.8, 6800, 520, -18.0, -2.0, -4.8],
  [125.5, 15.8, -1.4, 5600, -450, 11.0, 5.0, -3.8],
  [112.5, 14.2, 0.3, 5200, 60, 2.0, -17.0, -0.5],
  [124.5, 17.4, -0.9, 6350, -260, 7.0, 0.0, -4.1],
  [126.0, 19.0, 0.2, 6900, 40, 1.5, 6.0, -3.4],
  [124.5, 17.4, -0.4, 6300, -150, 1.6, 1.0, -4.0],
];

const round1 = (v) => Math.round(v * 10) / 10;

function shot([speed, vla, hla, back, side, toe, high, aoa]) {
  const axis = (Math.atan2(side, back) * 180) / Math.PI;
  const face = round1(hla * 1.15);
  return {
    ball: {
      Speed: speed, VLA: vla, HLA: hla, BackSpin: back, SideSpin: side,
      TotalSpin: Math.round(Math.hypot(back, side)), SpinAxis: round1(axis),
    },
    club: {
      Speed: round1(speed / 1.45), AngleOfAttack: aoa, FaceToTarget: face,
      Path: round1(face - 0.8 * axis), Loft: round1(vla + 5.0),
      HorizontalFaceImpact: toe, VerticalFaceImpact: high,
    },
  };
}

module.exports = {
  name: 'shots',
  needs: ['cameras'],
  ports: { web: 'free', monitor: 'free', gspro: 'free' },
  config: ({ ports }) => `default_units = "imperial"
chipping_clubs = ["GW", "SW", "LW"]
putting_clubs = ["PT"]

[simulation]
auto_chip = false

[replay]
keep_shots = 20
pre_roll_s = 1.12
post_roll_s = 0.6
start_before_impact_s = 1.12
clip_dir = "clips"
shot_cards = false

[webserver.0]
name = "Web Server"
bind = "127.0.0.1:${ports.web}"

[openconnect_server.0]
name = "Uneekor"
bind = "127.0.0.1:${ports.monitor}"
impact_lead_s = 0.0

[camera.0]
name = "Golfer Front"
camera = "test-pattern"
position = "golfer_front"
mode = "auto"

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
    const { fh, gspro } = ctx;
    await gspro.connected();
    await until('the club from GSPro', async () => (await fh.get('/api/status')).club === '7I');
    await until('the camera', async () => {
      const status = await fh.get('/api/status');
      return status.actors['camera.0'] && status.actors['camera.0'].status === 'connected';
    });
    const monitor = new Monitor(ctx.ports.monitor);
    ctx.cleanup.push(() => monitor.close());
    await monitor.connect();
    for (const [i, values] of SHOTS.entries()) {
      const { ball, club } = shot(values);
      await monitor.shot(i + 1, ball, club);
    }
    await gspro.received(SHOTS.length);
    await until('12 shots, the last with its clip', async () => {
      const shots = await fh.get('/api/shots?limit=50');
      const last = shots.find((s) => s.shot_number === SHOTS.length);
      return shots.length === SHOTS.length && last && (last.video || []).length > 0;
    });
  },

  snap: () => ({
    viewport: { width: 1600, height: 900 },
    images: [{
      name: 'shots',
      steps: [
        { click: [125, 27] }, // Shots tab
        { move: [800, 890] },
      ],
    }],
  }),
};
