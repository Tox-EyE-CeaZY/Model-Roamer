// Talking to the core: JSON over HTTP, live events over one web socket that reconnects on its own.

import { sleep } from './util.js';

export class ApiError extends Error {
  constructor(msg, status) {
    super(msg);
    this.status = status;
  }
}

let onAuth = () => {};
export const onUnauthorized = (fn) => (onAuth = fn);

export async function api(path, { method = 'GET', body, raw, timeout = 120000, quiet } = {}) {
  const ctl = new AbortController();
  const t = setTimeout(() => ctl.abort(), timeout);
  try {
    const headers = { 'X-Roamer': '1' };
    if (body !== undefined && !raw) headers['Content-Type'] = 'application/json';
    const r = await fetch(path.startsWith('/') ? path : '/api/' + path, {
      method,
      headers,
      body: raw ? body : body === undefined ? undefined : JSON.stringify(body),
      credentials: 'same-origin',
      signal: ctl.signal,
    });
    const text = await r.text();
    let v = null;
    try {
      v = text ? JSON.parse(text) : null;
    } catch {
      v = { error: text };
    }
    if (r.status === 401 && !quiet) onAuth();
    if (!r.ok) throw new ApiError(v?.error || `${r.status} ${r.statusText}`, r.status);
    return v;
  } catch (e) {
    if (e.name === 'AbortError') throw new ApiError('That took too long. Is the server still running?', 0);
    if (e instanceof ApiError) throw e;
    throw new ApiError("Can't reach Model Roamer. Is it still running?", 0);
  } finally {
    clearTimeout(t);
  }
}

export const get = (p, o) => api(p, o);
export const post = (p, body = {}, o) => api(p, { ...o, method: 'POST', body });
export const patch = (p, body = {}, o) => api(p, { ...o, method: 'PATCH', body });
export const del = (p, o) => api(p, { ...o, method: 'DELETE' });

/** Upload one file; resolves to {path, size}. */
export function upload(file) {
  return api('upload?name=' + encodeURIComponent(file.name), { method: 'POST', body: file, raw: true, timeout: 600000 });
}

// ------------------------------------------------------------ live events
const subs = new Set();
/** fn(event) for every event; returns an unsubscribe function. */
export function listen(fn) {
  subs.add(fn);
  return () => subs.delete(fn);
}
let ws = null, alive = false, tries = 0;
const statusSubs = new Set();
export const onSocket = (fn) => (statusSubs.add(fn), fn(alive), () => statusSubs.delete(fn));
const setAlive = (v) => {
  if (v === alive) return;
  alive = v;
  statusSubs.forEach((f) => f(v));
};

export function connect() {
  const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
  ws = new WebSocket(`${proto}//${location.host}/api/ws`);
  ws.onopen = () => {
    const back = tries > 0;
    tries = 0;
    setAlive(true);
    // anything could have changed while we were away
    if (back) subs.forEach((f) => f({ kind: 'resync', data: {} }));
  };
  ws.onmessage = (m) => {
    let ev;
    try {
      ev = JSON.parse(m.data);
    } catch {
      return;
    }
    subs.forEach((f) => {
      try {
        f(ev);
      } catch (e) {
        console.error(e);
      }
    });
  };
  ws.onclose = async () => {
    setAlive(false);
    tries++;
    await sleep(Math.min(8000, 400 * 2 ** Math.min(tries, 5)));
    connect();
  };
  ws.onerror = () => ws.close();
}
