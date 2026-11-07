'use strict';
// One headless flighthook for a scene: its own config, data folder and home,
// on localhost ports the scene chose.

const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { start, stop, until, accepting, getJson, postJson } = require('./util');

class Flighthook {
  /**
   * @param {object} o
   * @param {string} o.bin     the flighthook binary
   * @param {string} o.dir     scratch folder (created; config, data, log)
   * @param {string} o.config  the config file's TOML
   * @param {number} o.port    the web server port the config binds
   * @param {object} [o.env]   extra environment (the test seams)
   */
  constructor({ bin, dir, config, port, env = {} }) {
    Object.assign(this, { bin, dir, config, port, env });
    this.home = path.join(dir, 'home');
    this.child = null;
  }

  /** The user's local data folder flighthook sees (`dirs::data_local_dir`). */
  dataDir() {
    return os.platform() === 'darwin'
      ? path.join(this.home, 'Library', 'Application Support')
      : path.join(this.home, '.local', 'share');
  }

  /** The body tracking models folder flighthook uses. */
  modelsDir() {
    return path.join(this.dataDir(), 'flighthook', 'models');
  }

  async start() {
    fs.mkdirSync(this.dataDir(), { recursive: true });
    const config = path.join(this.dir, 'config.toml');
    fs.writeFileSync(config, this.config);
    const log = fs.openSync(path.join(this.dir, 'flighthook.log'), 'w');
    // Relative paths in the config (the clip folder) are inside the scratch folder.
    this.child = start(this.bin, ['--headless', '--config', config], {
      cwd: this.dir,
      stdio: ['ignore', log, log],
      env: {
        ...process.env,
        HOME: this.home,
        XDG_DATA_HOME: path.join(this.home, '.local', 'share'),
        XDG_CONFIG_HOME: path.join(this.home, '.config'),
        XDG_CACHE_HOME: path.join(this.home, '.cache'),
        TZ: 'UTC',
        ...this.env,
      },
    });
    fs.closeSync(log);
    let exited = null;
    this.child.once('exit', (code, signal) => {
      exited = signal || code;
    });
    await until(`flighthook on port ${this.port}`, async () => {
      if (exited !== null) throw new Error(`flighthook exited (${exited}); see ${this.dir}/flighthook.log`);
      return accepting(this.port);
    }, { timeoutMs: 30000, everyMs: 50 });
  }

  get(p) {
    return getJson(this.port, p);
  }

  post(p, body) {
    return postJson(this.port, p, body);
  }

  async stop() {
    await stop(this.child);
    this.child = null;
  }
}

module.exports = { Flighthook };
