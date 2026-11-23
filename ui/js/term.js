// A terminal panel for installs and sign-ins: the step runs on the machine Model Roamer runs on, in a real terminal
// (a PTY), and shows up here with its colours and prompts. Typing goes through, so device codes and "press Enter"
// prompts work without opening a terminal yourself.

import { h } from './util.js';
import { icon } from './icons.js';
import { post, del, get, listen } from './api.js';
import { toast, dialog, holdOverlay } from './ui.js';
import { openExternal } from './native.js';

let loaded = null;
function loadXterm() {
  if (loaded) return loaded;
  loaded = new Promise((res, rej) => {
    const css = h('link', { rel: 'stylesheet', href: '/ui/vendor/xterm.css' });
    document.head.append(css);
    const s1 = h('script', { src: '/ui/vendor/xterm.js' });
    s1.onload = () => {
      const s2 = h('script', { src: '/ui/vendor/addon-fit.js' });
      s2.onload = () => res();
      s2.onerror = rej;
      document.head.append(s2);
    };
    s1.onerror = rej;
    document.head.append(s1);
  });
  return loaded;
}

const b64 = (s) => Uint8Array.from(atob(s), (c) => c.charCodeAt(0));

/**
 * Run a setup step in a terminal panel. what: install | update | login | logout | connect | tool | tool-login.
 * Resolves when the panel closes, with the exit code (or null when it was stopped).
 */
export async function runStep({ what, agent, step = 0, title }) {
  let started;
  try {
    started = await post('terms', { what, agent, step, cols: 100, rows: 26 });
  } catch (e) {
    toast(e.message, { err: true, ms: 8000 });
    return null;
  }
  return showTerm(started.id, { title: title || started.label, cmd: started.cmd });
}

export async function showTerm(id, { title, cmd = [] }) {
  await loadXterm();
  const cs = getComputedStyle(document.documentElement);
  const term = new window.Terminal({
    fontFamily: "'JetBrains Mono', 'Cascadia Mono', Consolas, monospace",
    fontSize: 13,
    cursorBlink: true,
    convertEol: false,
    scrollback: 5000,
    theme: { background: cs.getPropertyValue('--bg').trim() ? '#0b1214' : '#000', foreground: '#dfe9e4', cursor: '#c8ff3d', selectionBackground: '#3a5a50' },
  });
  const fit = new window.FitAddon.FitAddon();
  term.loadAddon(fit);
  // links in the output (sign-in pages, device-code addresses) open in the person's browser
  term.registerLinkProvider?.({
    provideLinks(y, cb) {
      const line = term.buffer.active.getLine(y - 1)?.translateToString(true) || '';
      const out = [];
      for (const m of line.matchAll(/https?:\/\/[^\s'"<>]+/g)) out.push({ range: { start: { x: m.index + 1, y }, end: { x: m.index + m[0].length, y } }, text: m[0], activate: () => openExternal(m[0]) });
      cb(out);
    },
  });
  const host = h('div', { class: 'term-host' });
  const state = h('span', { class: 'status live' }, h('i', { class: 'pulse' }), 'Running');
  let exit = undefined;
  const stopBtn = h('button', { class: 'btn sm danger', onclick: () => stop() }, 'Stop');
  const closeBtn = h('button', { class: 'btn sm', onclick: () => close() }, 'Close');
  const box = h('div', { class: 'term-dlg cut' },
    h('div', { class: 'term-h' }, h('i', { html: icon('terminal', 16) }), h('b', {}, title), state, h('span', { class: 'grow' }), stopBtn, closeBtn),
    cmd.length ? h('div', { class: 'term-cmd mono' }, '$ ' + cmd.join(' ')) : null,
    host,
    h('div', { class: 'term-foot mono' }, 'Type here if it asks for something. Links open in your browser. Copy with Ctrl+Shift+C.'));
  const back = h('div', { class: 'dlg-back term-back' }, box);
  document.body.append(back);
  holdOverlay('term', true);
  requestAnimationFrame(() => back.classList.add('in'));
  term.open(host);
  const doFit = () => {
    try {
      fit.fit();
      post(`terms/${id}/resize`, { cols: term.cols, rows: term.rows }).catch(() => {});
    } catch { /* not laid out yet */ }
  };
  setTimeout(doFit, 60);
  const ro = new ResizeObserver(() => doFit());
  ro.observe(host);
  term.focus();
  term.attachCustomKeyEventHandler((e) => {
    if (e.type === 'keydown' && e.ctrlKey && e.shiftKey && e.key.toLowerCase() === 'c') {
      navigator.clipboard?.writeText(term.getSelection());
      return false;
    }
    if (e.type === 'keydown' && e.ctrlKey && e.shiftKey && e.key.toLowerCase() === 'v') {
      navigator.clipboard?.readText().then((t) => post(`terms/${id}/input`, { data: t }));
      return false;
    }
    return true;
  });
  term.onData((d) => exit === undefined && post(`terms/${id}/input`, { data: d }).catch(() => {}));
  // whatever already printed (we may have opened late)
  try {
    const buf = await get('terms/' + id);
    if (buf.data) term.write(b64(buf.data));
    if (buf.exited != null) finished(buf.exited);
  } catch { /* fine */ }
  let resolve;
  const done = new Promise((r) => (resolve = r));
  const un = listen((ev) => {
    if (ev.kind !== 'term' || ev.data?.id !== id) return;
    if (ev.data.data) term.write(b64(ev.data.data));
    if (ev.data.exit !== undefined) finished(ev.data.exit);
  });
  function finished(code) {
    if (exit !== undefined) return;
    exit = code;
    state.className = 'status ' + (code === 0 ? 'acc' : 'hot');
    state.replaceChildren(code === 0 ? 'Finished' : `Ended (code ${code})`);
    stopBtn.hidden = true;
    closeBtn.classList.add('primary');
    term.write(`\r\n\x1b[2m— ${code === 0 ? 'done' : 'ended with code ' + code}. Close this panel when you’re ready.\x1b[0m\r\n`);
  }
  async function stop() {
    await del('terms/' + id).catch(() => {});
    finished(-1);
  }
  async function close() {
    if (exit === undefined) {
      const ok = await dialog({ title: 'Still running', text: 'Stop it and close the panel?', ok: 'Stop and close', danger: true });
      if (!ok) return;
      await del('terms/' + id).catch(() => {});
    } else del('terms/' + id).catch(() => {});
    un();
    ro.disconnect();
    term.dispose();
    document.removeEventListener('keydown', esc, true);
    holdOverlay('term', false);
    back.remove();
    resolve(exit === undefined || exit === -1 ? null : exit);
  }
  const esc = (e) => {
    if (e.key === 'Escape' && exit !== undefined) {
      e.preventDefault();
      close();
    }
  };
  document.addEventListener('keydown', esc, true);
  return done;
}
