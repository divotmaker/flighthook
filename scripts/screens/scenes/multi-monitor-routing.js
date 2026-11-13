'use strict';
// screens/multi-monitor-routing.png: a GSPro output's page, its settings
// routing full swings to the Mock and chips to an Open Connect monitor,
// putts to any. Cropped to the page's header, telemetry and settings rows.

const { until } = require('../lib/util');
const { Simulator } = require('../lib/openconnect');

module.exports = {
  name: 'multi-monitor-routing',
  ports: { web: 'free', monitor: 'free', gspro: 5975 },
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
name = "Mock Monitor"

[openconnect_server.0]
name = "Open Connect"
bind = "127.0.0.1:${ports.monitor}"

[openconnect_client.0]
name = "Local GSPro"
address = "127.0.0.1:${ports.gspro}"
full_monitor = "mock_monitor.0"
chipping_monitor = "openconnect_server.0"
`,

  async before(ctx) {
    ctx.gspro = new Simulator(ctx.ports.gspro);
    ctx.cleanup.push(() => ctx.gspro.close());
    await ctx.gspro.listen();
  },

  async drive(ctx) {
    await until('GSPro connected', async () => {
      const { cards: actors } = await ctx.fh.get('/api/status');
      return actors['openconnect_client.0'] && actors['openconnect_client.0'].status === 'connected';
    });
  },

  snap: () => ({
    viewport: { width: 1600, height: 1400 },
    images: [{
      name: 'multi-monitor-routing',
      steps: [
        { click: [124, 26] }, // Settings
        { click: [134, 477] }, // Local GSPro's row
        { move: [1590, 1390] },
      ],
      crop: { card: [300, 300], margin: 2, height: 300 },
    }],
  }),
};
