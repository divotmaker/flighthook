'use strict';
// Shared helpers: logging, timing, child processes (all stopped on exit),
// ports and small HTTP calls.

const { spawn } = require('node:child_process');
const fs = require('node:fs');
const http = require('node:http');
const net = require('node:net');

const children = new Set();

/** Spawn a child that is killed when the runner exits, however it exits. */
function start(cmd, args, opts = {}) {
  const child = spawn(cmd, args, { stdio: ['ignore', 'pipe', 'pipe'], ...opts });
  children.add(child);
  child.on('exit', () => children.delete(child));
  return child;
}

/** Run a command to completion; rejects with its output tail on failure. */
function run(cmd, args, opts = {}) {
  return new Promise((resolve, reject) => {
    const child = start(cmd, args, opts);
    const out = [];
    const keep = (d) => {
      out.push(d);
      if (opts.log) opts.log.write(d);
    };
    child.stdout.on('data', keep);
    child.stderr.on('data', keep);
    child.on('error', reject);
    child.on('exit', (code, signal) => {
      const text = Buffer.concat(out).toString();
      if (code === 0) resolve(text);
      else {
        const tail = text.trim().split('\n').slice(-20).join('\n');
        reject(new Error(`${cmd} ${args.join(' ')} failed (${signal || code})\n${tail}`));
      }
    });
  });
}

/** Stop a child: SIGTERM, then SIGKILL after `graceMs`. */
function stop(child, graceMs = 3000) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return Promise.resolve();
  return new Promise((resolve) => {
    const timer = setTimeout(() => child.kill('SIGKILL'), graceMs);
    child.once('exit', () => {
      clearTimeout(timer);
      resolve();
    });
    child.kill('SIGTERM');
  });
}

function killAll() {
  for (const child of children) {
    try {
      child.kill('SIGKILL');
    } catch {
      // already gone
    }
  }
}

process.on('exit', killAll);
for (const signal of ['SIGINT', 'SIGTERM', 'SIGHUP']) {
  process.on(signal, () => {
    killAll();
    process.exit(130);
  });
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** Poll `fn` until it returns a truthy value; rejects after `timeoutMs`. */
async function until(what, fn, { timeoutMs = 60000, everyMs = 100 } = {}) {
  const end = Date.now() + timeoutMs;
  for (;;) {
    const value = await fn();
    if (value) return value;
    if (Date.now() > end) throw new Error(`timed out waiting for ${what}`);
    await sleep(everyMs);
  }
}

/** A free localhost port, as the OS hands one out. */
function freePort() {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => {
      const { port } = server.address();
      server.close(() => resolve(port));
    });
  });
}

/** Whether `port` can be listened on at 127.0.0.1. */
function portFree(port) {
  return new Promise((resolve) => {
    const server = net.createServer();
    server.once('error', () => resolve(false));
    server.listen(port, '127.0.0.1', () => server.close(() => resolve(true)));
  });
}

/** Whether something accepts TCP connections on `port` (no request is sent). */
function accepting(port) {
  return new Promise((resolve) => {
    const socket = net.connect(port, '127.0.0.1');
    socket.once('connect', () => {
      socket.destroy();
      resolve(true);
    });
    socket.once('error', () => resolve(false));
  });
}

/** One HTTP request to the local web server; resolves `{ status, body }`. */
function request(port, method, path, body) {
  return new Promise((resolve, reject) => {
    const data = body === undefined ? undefined : Buffer.from(JSON.stringify(body));
    const req = http.request(
      {
        host: '127.0.0.1',
        port,
        method,
        path,
        headers: data ? { 'content-type': 'application/json', 'content-length': data.length } : {},
      },
      (res) => {
        const parts = [];
        res.on('data', (d) => parts.push(d));
        res.on('end', () => resolve({ status: res.statusCode, body: Buffer.concat(parts) }));
      },
    );
    req.on('error', reject);
    req.setTimeout(30000, () => req.destroy(new Error(`${method} ${path} timed out`)));
    if (data) req.write(data);
    req.end();
  });
}

async function getJson(port, path) {
  const { status, body } = await request(port, 'GET', path);
  if (status !== 200) throw new Error(`GET ${path}: ${status} ${body}`);
  return JSON.parse(body.toString());
}

async function postJson(port, path, value) {
  const { status, body } = await request(port, 'POST', path, value);
  if (status >= 300) throw new Error(`POST ${path}: ${status} ${body}`);
  return body.length ? JSON.parse(body.toString()) : null;
}

/** Stage timings for one scene (or the build), in ms. */
class Timer {
  constructor() {
    this.stages = {};
    this.t0 = Date.now();
  }

  async stage(name, fn) {
    const t = Date.now();
    try {
      return await fn();
    } finally {
      this.stages[name] = (this.stages[name] || 0) + (Date.now() - t);
    }
  }

  total() {
    return Date.now() - this.t0;
  }
}

function rmrf(path) {
  fs.rmSync(path, { recursive: true, force: true });
}

module.exports = {
  start,
  run,
  stop,
  killAll,
  sleep,
  until,
  freePort,
  portFree,
  accepting,
  request,
  getJson,
  postJson,
  Timer,
  rmrf,
};
