'use strict';
// Open Connect over TCP: a launch monitor that sends shots to flighthook's
// Open Connect server, and a simulator stand-in that flighthook's GSPro
// output connects to. Both speak the stream of JSON objects Open Connect uses.

const net = require('node:net');
const { until } = require('./util');

/** Splits a TCP stream into JSON objects (by brace depth, outside strings). */
class JsonStream {
  constructor(onMessage) {
    this.onMessage = onMessage;
    this.buf = '';
    this.depth = 0;
    this.inString = false;
    this.escape = false;
  }

  push(chunk) {
    for (const ch of chunk.toString()) {
      if (this.depth === 0 && ch !== '{') continue;
      this.buf += ch;
      if (this.inString) {
        if (this.escape) this.escape = false;
        else if (ch === '\\') this.escape = true;
        else if (ch === '"') this.inString = false;
      } else if (ch === '"') this.inString = true;
      else if (ch === '{') this.depth += 1;
      else if (ch === '}' && --this.depth === 0) {
        const text = this.buf;
        this.buf = '';
        let value = null;
        try {
          value = JSON.parse(text);
        } catch {
          // not JSON: ignored
        }
        if (value) this.onMessage(value);
      }
    }
  }
}

const HEARTBEAT_MS = 3000;

/** A launch monitor connected to flighthook's Open Connect server. */
class Monitor {
  /**
   * @param {number} port        the Open Connect server's port
   * @param {object} [o]
   * @param {number} [o.localPort] the client port to connect from (it shows
   *                              on the server's card as `peer`)
   * @param {string} [o.deviceId]
   */
  constructor(port, { localPort, deviceId = 'OC' } = {}) {
    Object.assign(this, { port, localPort, deviceId });
    this.replies = [];
    this.socket = null;
    this.timer = null;
  }

  async connect() {
    this.socket = await new Promise((resolve, reject) => {
      const opts = { host: '127.0.0.1', port: this.port };
      if (this.localPort) Object.assign(opts, { localAddress: '127.0.0.1', localPort: this.localPort });
      const socket = net.connect(opts);
      socket.once('connect', () => resolve(socket));
      socket.once('error', reject);
    });
    this.socket.on('error', () => {});
    const stream = new JsonStream((m) => this.replies.push(m));
    this.socket.on('data', (d) => stream.push(d));
    this.heartbeat();
    this.timer = setInterval(() => this.heartbeat(), HEARTBEAT_MS);
  }

  send(message) {
    this.socket.write(JSON.stringify(message));
  }

  heartbeat() {
    this.send({
      DeviceID: this.deviceId,
      Units: 'Yards',
      ShotNumber: 0,
      APIversion: '1',
      ShotDataOptions: {
        ContainsBallData: false,
        ContainsClubData: false,
        LaunchMonitorIsReady: true,
        LaunchMonitorBallDetected: true,
        IsHeartBeat: true,
      },
    });
  }

  /** Send shot `n` with `ball` (and `club`) data; resolves on the server's reply. */
  async shot(n, ball, club) {
    const seen = this.replies.length;
    this.send({
      DeviceID: this.deviceId,
      Units: 'Yards',
      ShotNumber: n,
      APIversion: '1',
      BallData: ball,
      ...(club ? { ClubData: club } : {}),
      ShotDataOptions: { ContainsBallData: true, ContainsClubData: Boolean(club) },
    });
    await until(`the reply to shot ${n}`, () =>
      this.replies.slice(seen).some((r) => r.Code === 200 && /shot/i.test(r.Message || '')),
    { timeoutMs: 10000, everyMs: 10 });
  }

  close() {
    clearInterval(this.timer);
    if (this.socket) this.socket.destroy();
    this.socket = null;
  }
}

/** A simulator stand-in on `port`: sends player info on connect, answers shots with 200. */
class Simulator {
  constructor(port, { handed = 'RH', club = 'I7' } = {}) {
    Object.assign(this, { port, handed, club });
    this.shots = 0;
    this.heartbeats = 0;
    this.sockets = new Set();
  }

  listen() {
    this.server = net.createServer((socket) => {
      this.sockets.add(socket);
      socket.on('close', () => this.sockets.delete(socket));
      socket.on('error', () => {});
      socket.write(JSON.stringify({
        Code: 201,
        Message: 'GSPro Player Information',
        Player: { Handed: this.handed, Club: this.club },
      }));
      const stream = new JsonStream((m) => {
        const options = m.ShotDataOptions || {};
        if (options.ContainsBallData) {
          this.shots += 1;
          socket.write('{"Code":200,"Message":"Ball Data received"}');
        } else if (options.IsHeartBeat) {
          this.heartbeats += 1;
        }
      });
      socket.on('data', (d) => stream.push(d));
    });
    return new Promise((resolve, reject) => {
      this.server.once('error', reject);
      this.server.listen(this.port, '127.0.0.1', resolve);
    });
  }

  /** Resolves once flighthook is connected. */
  connected() {
    return until(`flighthook's GSPro output on port ${this.port}`, () => this.sockets.size > 0,
      { timeoutMs: 30000, everyMs: 20 });
  }

  /**
   * Resolves at flighthook's first heartbeat. Its GSPro output reports itself
   * connected in the same pass (one second after connecting).
   */
  heartbeat() {
    return until(`a heartbeat at the simulator on port ${this.port}`, () => this.heartbeats > 0,
      { timeoutMs: 30000, everyMs: 10 });
  }

  /** Resolves once `n` shots have arrived. */
  received(n) {
    return until(`${n} shots at the simulator`, () => this.shots >= n, { timeoutMs: 30000, everyMs: 10 });
  }

  close() {
    for (const s of this.sockets) s.destroy();
    if (this.server) this.server.close();
  }
}

module.exports = { Monitor, Simulator, JsonStream };
