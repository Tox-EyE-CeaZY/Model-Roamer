// The shell: title bar, side rail, the router that swaps screens, keyboard shortcuts and the command palette.

import { h, $, combo, debounce } from './util.js';
import { icon, MARK } from './icons.js';
import { initTopo } from './bg.js';
import { connect, onSocket, onUnauthorized, listen, get, post } from './api.js';
import { S, load, watch, project as projectOf } from './store.js';
import { isDesktop, isPop, plat, desk, win, native, onNative, setTitle, openExternal } from './native.js';
import { toast, dialog, showMenu, overlaid, closeMenu } from './ui.js';
import { ic, stamp, statusChip, loading, errorBox } from './common.js';

const VIEWS = {
  home: () => import('./views/home.js'),
  project: () => import('./views/project.js'),
  thread: () => import('./views/thread.js'),
  chats: () => import('./views/chats.js'),
  agents: () => import('./views/agents.js'),
  artifacts: () => import('./views/artifacts.js'),
  artifact: () => import('./views/artifacts.js').then((m) => ({ default: m.viewer })),
  memory: () => import('./views/memory.js'),
  usage: () => import('./views/usage.js'),
  settings: () => import('./views/settings.js'),
  pair: () => import('./views/pair.js'),
  welcome: () => import('./views/welcome.js'),
  browse: () => import('./views/browse.js'),
};

const NAV = [
  ['', 'home', 'Home', 'c+1'],
  ['chats', 'chat', 'Chats', 'c+2'],
  ['agents', 'agents', 'Agents', 'c+3'],
  ['artifacts', 'artifact', 'Artifacts', 'c+4'],
  ['memory', 'memory', 'Memory', 'c+5'],
  ['usage', 'chart', 'Usage', 'c+6'],
  ['settings', 'settings', 'Settings', 'c+,'],
];

export function parseRoute(hash = location.hash) {
  const [path, qs] = hash.replace(/^#\/?/, '').split('?');
  const parts = path.split('/').filter(Boolean).map(decodeURIComponent);
  const q = Object.fromEntries(new URLSearchParams(qs || ''));
  const [a, b] = parts;
  if (!a) return { view: 'home', q };
  if (a === 'p' && b) return { view: 'project', id: b, tab: parts[2] || '', q };
  if (a === 't' && b) return { view: 'thread', id: b, q };
  if (a === 'a' && b) return { view: 'artifact', id: b, q };
  if (a === 'settings') return { view: 'settings', section: b || '', q };
  if (VIEWS[a]) return { view: a, q };
  return { view: 'home', q };
}

export const go = (hash) => {
  if (location.hash === hash) render();
  else location.hash = hash;
};

// Where we are in this window's history: each page we open is stamped with its place, so "back" can tell a page we
// came from inside the app (go back to it, exactly as it was) from the first one (go to the fallback instead).
let navAt = 0, navStarted = false;
function stampNav() {
  try {
    if (history.state?.i == null) history.replaceState({ ...(history.state || {}), i: navStarted ? navAt + 1 : 0 }, '');
    navAt = history.state?.i ?? navAt;
    navStarted = true;
  } catch { /* fine */ }
}
stampNav();
addEventListener('hashchange', stampNav);
addEventListener('popstate', stampNav);
/** Back to the page we came from; with nowhere to go back to (opened straight on this page), to the fallback. */
export function back(fallback = '#/') {
  if (navAt > 0) history.back();
  else go(fallback);
}

/** Open a thread: in this window, or in one of its own when the person likes pop-outs (or asks for one). */
export function openThread(id, { pop } = {}) {
  if ((pop ?? S.settings.popout_threads) && !isPop) {
    import('./native.js').then((n) => n.popOut('#/t/' + id, { key: 't:' + id }));
  } else go('#/t/' + id);
}

// ------------------------------------------------------------ title bar
const remote = isDesktop && desk.local && !location.href.startsWith(desk.local);
function titlebar() {
  const bar = $('#titlebar');
  const chrome = isDesktop && plat !== 'macos';
  const drag = isDesktop
    ? {
      onmousedown: (e) => e.button === 0 && e.detail === 1 && e.target.closest('.tb-drag') && !e.target.closest('button') && win('drag'),
      ondblclick: (e) => !e.target.closest('button') && win('max'),
    }
    : {};
  const maxBtn = h('button', { class: 'tb-btn', title: 'Maximize', onclick: () => win('max'), html: icon('winMax', 14) });
  onNative('win', (m) => {
    document.documentElement.classList.toggle('maxed', !!m.max);
    document.documentElement.classList.toggle('fullscreen', !!m.fs);
    maxBtn.innerHTML = icon(m.max ? 'winRestore' : 'winMax', 14);
    maxBtn.title = m.max ? 'Restore' : 'Maximize';
  });
  const where = h('button', { class: 'tb-where', type: 'button', onclick: (e) => serverMenu(e.currentTarget) });
  const drawWhere = () => {
    const isRemote = remote || (!S.me.local && S.ready);
    where.replaceChildren(ic(isRemote ? 'server' : 'desktop', 13), h('span', {}, isRemote ? S.server.name || 'Server' : 'This computer'));
    where.classList.toggle('remote', isRemote);
  };
  watch(drawWhere);
  drawWhere();
  const live = h('i', { class: 'tb-live', title: 'Connected' });
  onSocket((ok) => {
    live.classList.toggle('off', !ok);
    live.title = ok ? 'Live' : 'Reconnecting…';
  });
  const popTitle = h('span', { class: 'tb-pop-title' });
  bar.append(
    h('div', { class: 'tb-drag tb-left', ...drag },
      h('a', { class: 'tb-mark', href: isPop ? null : '#/', title: 'Home', html: `<svg viewBox="0 0 40 40" width="17" height="17">${MARK}</svg>` }),
      h('span', { class: 'tb-name' }, 'MODEL', h('i', {}, '/'), 'ROAMER'),
      isPop ? popTitle : null),
    h('div', { class: 'tb-drag tb-fill', ...drag }),
    isPop ? null : h('button', { class: 'tb-search', type: 'button', onclick: () => palette() }, ic('search', 14), h('span', {}, 'Search or jump to…'), h('kbd', {}, plat === 'macos' ? '⌘K' : 'Ctrl K')),
    h('div', { class: 'tb-drag tb-fill', ...drag }),
    isPop ? null : where,
    live,
    isPop && isDesktop ? h('button', { class: 'tb-btn', title: 'Main window', onclick: () => native('focus_main'), html: icon('home', 14) }) : null,
    chrome
      ? h('div', { class: 'tb-btns' },
        h('button', { class: 'tb-btn', title: 'Minimize', onclick: () => win('min'), html: icon('winMin', 14) }),
        maxBtn,
        h('button', { class: 'tb-btn close', title: 'Close', onclick: () => win('close'), html: icon('close', 14) }))
      : null,
  );
  document.documentElement.classList.toggle('desktop', isDesktop);
  document.documentElement.classList.toggle('pop', isPop);
  document.documentElement.dataset.plat = plat;
  if (chrome) for (const d of ['N', 'S', 'E', 'W', 'NE', 'NW', 'SE', 'SW']) document.body.append(h('i', { class: 'rz rz-' + d, onmousedown: (e) => (e.preventDefault(), win('resize:' + d)) }));
  if (isDesktop) native('state');
  return { popTitle };
}

async function serverMenu(anchor) {
  const items = [{ header: 'Where the work runs' }];
  const isRemote = remote || !S.me.local;
  items.push({ label: isRemote ? `${S.server.name || 'Server'} (connected)` : 'This computer', icon: isRemote ? 'server' : 'desktop', checked: true, disabled: true });
  if (remote) items.push({ label: 'Back to this computer', icon: 'desktop', run: () => native('home') });
  if (S.me.local) {
    let conns = [];
    try {
      conns = await get('connections');
    } catch { /* fine */ }
    for (const c of conns) items.push({ label: c.name, icon: 'server', kbd: c.via, run: () => switchTo(c) });
    items.push({ sep: true }, { label: 'Connect to a server…', icon: 'plus', run: () => go('#/settings/remote') });
    items.push({ label: 'Let other devices in…', icon: 'qr', run: () => go('#/settings/remote') });
  }
  const r = anchor.getBoundingClientRect();
  showMenu(items, r.right, r.bottom + 4, { anchor });
}

export async function switchTo(c) {
  try {
    const r = await post(`connections/${c.id}/open`);
    if (isDesktop) native('navigate', { url: r.url });
    else location.href = r.url;
  } catch (e) {
    toast(e.message, { err: true });
  }
}

// ------------------------------------------------------------ side rail
function side() {
  const nav = $('#side');
  if (isPop) return nav.remove();
  const draw = () => {
    const r = parseRoute();
    const cur = r.view === 'project' || r.view === 'thread' ? '' : r.view === 'home' ? '' : r.view === 'artifact' ? 'artifacts' : r.view;
    const working = S.projects.reduce((n, p) => n + (p.working || 0), 0);
    const items = NAV.map(([path, ico, label, k]) =>
      h('a', { class: 'sd-i' + (cur === path && (path !== '' || r.view === 'home') ? ' on' : ''), href: '#/' + path, title: `${label} (${k.replace('c+', 'Ctrl+').replace(',', ',')})` },
        ic(ico, 17), h('span', {}, label),
        path === 'agents' && Object.values(S.agents).some((a) => a.limit?.active) ? h('i', { class: 'sd-dot hot', title: 'An agent is at its usage limit' }) : null,
        path === '' && working ? h('b', { class: 'sd-n', title: `${working} working` }, working) : null));
    const projects = S.projects.map((p) =>
      h('a', { class: 'sd-p' + (r.view === 'project' && r.id === p.id ? ' on' : ''), href: '#/p/' + p.id, style: { '--ph': p.color }, title: p.name, oncontextmenu: (e) => (e.preventDefault(), projectMenu(p, e.clientX, e.clientY)) },
        h('i', { class: 'sd-sw' }),
        h('span', {}, p.name),
        p.decisions ? h('b', { class: 'sd-n hot', title: 'Waiting on you' }, p.decisions) : p.working ? h('i', { class: 'pulse', title: `${p.working} working` }) : p.unread ? h('i', { class: 'sd-dot', title: 'Unread' }) : null));
    nav.replaceChildren(
      h('div', { class: 'sd-nav' }, items),
      h('div', { class: 'sd-head' }, h('span', {}, 'Projects'), h('button', { class: 'ibtn sm', title: 'New project (Ctrl+Shift+N)', onclick: () => newProject(), html: icon('plus', 14) })),
      h('div', { class: 'sd-projects' }, projects.length ? projects : h('button', { class: 'sd-empty', onclick: () => newProject() }, 'No projects yet. Make one →')),
      h('div', { class: 'sd-foot' },
        h('button', { class: 'sd-collapse ibtn sm', title: 'Collapse the side rail (Ctrl+B)', onclick: () => toggleRail(), html: icon('left', 14) }),
        h('span', { class: 'mono faint' }, 'v' + (S.version || ''))),
    );
  };
  watch(draw);
  addEventListener('hashchange', draw);
  draw();
  try {
    if (localStorage.getItem('mr-rail') === 'min') document.documentElement.classList.add('rail-min');
  } catch { /* fine */ }
}
function toggleRail() {
  const min = document.documentElement.classList.toggle('rail-min');
  try {
    localStorage.setItem('mr-rail', min ? 'min' : '');
  } catch { /* fine */ }
}

export function projectMenu(p, x, y) {
  showMenu([
    { label: 'Open', icon: 'board', run: () => go('#/p/' + p.id) },
    { label: 'New thread…', icon: 'plus', run: () => import('./views/project.js').then((m) => m.newThreadDialog(p.id)) },
    { label: 'Project settings…', icon: 'settings', run: () => go('#/p/' + p.id + '/settings') },
    { sep: true },
    { label: 'Delete project', icon: 'trash', danger: true, run: () => deleteProject(p) },
  ], x, y);
}

export async function deleteProject(p) {
  const ok = await dialog({ title: `Delete ${p.name}?`, text: 'Its threads stop and it leaves the list. Files on disk stay where they are. You can undo this for a little while.', ok: 'Delete', danger: true });
  if (!ok) return;
  try {
    await (await import('./api.js')).del('projects/' + p.id);
    if (location.hash.includes(p.id)) go('#/');
    toast(`Deleted ${p.name}`, { action: 'Undo', ms: 9000, run: () => post(`projects/${p.id}/restore`).then(() => toast('Restored')) });
  } catch (e) {
    toast(e.message, { err: true });
  }
}

export function newProject() {
  return import('./views/home.js').then((m) => m.newProjectDialog());
}

// ------------------------------------------------------------ router
let cleanup = null, seq = 0, routeHook = null, lastRoute = null;
let tb;
async function render() {
  const r = parseRoute();
  // inside one project's board, opening or closing a thread beside the chat doesn't rebuild the page
  const prev = lastRoute;
  lastRoute = r;
  if (routeHook && prev && prev.view === 'project' && r.view === 'project' && prev.id === r.id && prev.tab === r.tab) {
    closeMenu();
    return routeHook(r);
  }
  routeHook = null;
  const my = ++seq;
  const el = $('#view');
  closeMenu();
  try {
    cleanup?.();
  } catch (e) {
    console.error(e);
  }
  cleanup = null;
  // first run: a short welcome before anything else
  if (S.ready && !isPop && S.me.local && !S.settings.welcomed && r.view === 'home' && !S.projects.length) r.view = 'welcome';
  el.replaceChildren(loading());
  el.scrollTop = 0;
  try {
    const mod = await VIEWS[r.view]();
    if (my !== seq) return;
    const box = h('div', { class: 'v v-' + r.view });
    el.replaceChildren(box);
    cleanup = (await mod.default(box, r, { setPopTitle: (t) => tb?.popTitle && (tb.popTitle.textContent = t), onRoute: (fn) => (routeHook = fn) })) || null;
    if (r.view !== 'thread' && r.view !== 'project') setTitle(r.view === 'home' ? 'Model Roamer' : `${r.view[0].toUpperCase() + r.view.slice(1)} · Model Roamer`);
  } catch (e) {
    console.error(e);
    if (my === seq) el.replaceChildren(h('div', { class: 'v' }, errorBox(e, render)));
  }
}

// ------------------------------------------------------------ command palette
export async function palette(initial = '') {
  if (document.querySelector('.pal')) return;
  const inp = h('input', { class: 'pal-in', placeholder: 'Search threads, projects, memory — or type a command', value: initial, spellcheck: 'false' });
  const list = h('div', { class: 'pal-list' });
  const back = h('div', { class: 'dlg-back in pal-back', onpointerdown: (e) => e.target === back && close() });
  const box = h('div', { class: 'pal cut' }, h('div', { class: 'pal-top' }, ic('search', 16), inp, h('kbd', {}, 'Esc')), list);
  back.append(box);
  document.body.append(back);
  let rows = [], cur = 0;
  const actions = [
    { label: 'New chat', hint: 'Ctrl+N', icon: 'chat', run: () => import('./views/chats.js').then((m) => m.newChat()) },
    { label: 'New project', hint: 'Ctrl+Shift+N', icon: 'board', run: () => newProject() },
    { label: 'Agents and sign-in', icon: 'agents', run: () => go('#/agents') },
    { label: 'Routing rules', icon: 'route', run: () => go('#/agents?tab=routing') },
    { label: 'Usage and limits', icon: 'chart', run: () => go('#/usage') },
    { label: 'Memory', icon: 'memory', run: () => go('#/memory') },
    { label: 'Artifacts', icon: 'artifact', run: () => go('#/artifacts') },
    { label: 'Remote access and devices', icon: 'wifi', run: () => go('#/settings/remote') },
    { label: 'GitHub', icon: 'branch', run: () => go('#/settings/github') },
    { label: 'Appearance', icon: 'brush', run: () => go('#/settings/look') },
    { label: 'Keyboard shortcuts', hint: '?', icon: 'keyboard', run: () => shortcuts() },
    { label: 'Check for updates', icon: 'download', run: () => go('#/settings/about') },
    isDesktop ? { label: 'Developer tools', icon: 'code', run: () => native('devtools') } : null,
  ].filter(Boolean);
  const draw = (items) => {
    rows = items;
    cur = Math.min(cur, Math.max(0, rows.length - 1));
    list.replaceChildren(...(rows.length ? rows.map((r, i) => r.header ? h('div', { class: 'pal-h' }, r.header) : h('button', { class: 'pal-i' + (i === cur ? ' hi' : ''), type: 'button', onmouseenter: () => ((cur = i), mark()), onclick: () => pick(i) }, ic(r.icon || 'right', 15), h('span', { class: 'pal-t' }, r.label), r.sub ? h('small', {}, r.sub) : null, r.node || null, r.hint ? h('kbd', {}, r.hint) : null)) : [h('div', { class: 'pal-none' }, 'Nothing matches.')]));
    if (rows[cur]?.header) move(1);
  };
  const mark = () => list.querySelectorAll('.pal-i, .pal-h').forEach((b, i) => b.classList?.toggle('hi', i === cur));
  const move = (d) => {
    if (!rows.length) return;
    let i = cur;
    do i = (i + d + rows.length) % rows.length;
    while (rows[i].header && i !== cur);
    cur = i;
    mark();
    list.children[cur]?.scrollIntoView({ block: 'nearest' });
  };
  const pick = (i) => {
    const r = rows[i];
    if (!r || r.header) return;
    close();
    r.run();
  };
  const search = debounce(async (q) => {
    const ql = q.toLowerCase();
    const acts = actions.filter((a) => !ql || a.label.toLowerCase().includes(ql));
    const projs = S.projects.filter((p) => !ql || p.name.toLowerCase().includes(ql)).slice(0, 6).map((p) => ({ label: p.name, sub: 'project', icon: 'board', run: () => go('#/p/' + p.id) }));
    let found = [];
    if (q.trim().length >= 2) {
      try {
        const r = await get('search?q=' + encodeURIComponent(q));
        // where a thread opens: a chat on its own, anything in a project beside that project's chat
        const open = (t, msg) => {
          try {
            if (msg) sessionStorage.setItem('mr-jump', msg + '|' + Date.now());
          } catch { /* fine */ }
          if (t.kind === 'chat' || !t.project_id) return go('#/t/' + (t.id || t.thread_id));
          go('#/p/' + t.project_id + (t.kind === 'lead' ? '' : '?t=' + (t.id || t.thread_id)));
        };
        const clean = (x) => (x || '').replace(/\s+/g, ' ').trim();
        const where = (t) => (t.kind === 'chat' ? 'chat' : projectOf(t.project_id)?.name || 'thread') + (t.kind === 'lead' ? ' · project chat' : '');
        found = [
          ...r.projects.map((p) => ({ label: p.name, sub: clean(p.snippet) || 'project', icon: 'board', run: () => go('#/p/' + p.id) })),
          ...r.threads.map((t) => ({ label: t.title, sub: clean(t.snippet) || where(t), icon: t.kind === 'chat' ? 'chat' : 'thread', node: statusChip(t), run: () => open(t) })),
          ...r.messages.slice(0, 12).map((m) => ({ label: clean(m.snippet), sub: `${m.role === 'agent' ? 'agent' : m.author || 'you'} · ${where(m)}`, icon: 'hash', run: () => open(m, m.id) })),
          ...r.artifacts.map((a) => ({ label: a.title, sub: clean(a.description) || a.kind + (projectOf(a.project_id) ? ' · ' + projectOf(a.project_id).name : ''), icon: 'artifact', run: () => go('#/a/' + a.id) })),
          ...r.memory.slice(0, 6).map((m) => ({ label: m.name, sub: m.description || 'memory', icon: 'memory', run: () => go('#/memory?open=' + m.id) })),
        ];
      } catch { /* offline: actions still work */ }
    }
    const out = [];
    if (projs.length) out.push({ header: 'Projects' }, ...projs);
    if (found.length) out.push({ header: 'Found' }, ...found);
    if (acts.length) out.push({ header: 'Go to' }, ...acts);
    cur = 0;
    draw(out);
  }, 140);
  inp.oninput = () => search(inp.value);
  inp.onkeydown = (e) => {
    if (e.key === 'ArrowDown') move(1);
    else if (e.key === 'ArrowUp') move(-1);
    else if (e.key === 'Enter') pick(cur);
    else if (e.key === 'Escape') close();
    else return;
    e.preventDefault();
  };
  function close() {
    back.remove();
  }
  search.flush(initial);
  setTimeout(() => inp.focus(), 10);
}

function shortcuts() {
  const k = (keys, what) => h('div', { class: 'sc-row' }, h('span', {}, what), h('kbd', {}, keys));
  dialog({
    title: 'Keyboard shortcuts',
    wide: true,
    ok: 'Got it',
    cancel: null,
    body: h('div', { class: 'sc-grid' },
      k('Ctrl K', 'Search and jump anywhere'),
      k('Ctrl N', 'New chat'),
      k('Ctrl Shift N', 'New project'),
      k('Ctrl 1 … 6', 'Home, Chats, Agents, Artifacts, Memory, Usage'),
      k('Ctrl ,', 'Settings'),
      k('Ctrl B', 'Collapse the side rail'),
      k('Ctrl Enter', 'Send a message'),
      k('Ctrl Shift O', 'Pop the thread out into its own window'),
      k('Ctrl .', 'Stop the agent in this thread'),
      k('Alt ←', 'Back'),
      k('Ctrl + / −', 'Zoom'),
      k('F11', 'Full screen'),
      k('?', 'This list')),
  });
}

let zoom = 1;
function keys(e) {
  if (document.querySelector('.pal')) return;
  const c = combo(e);
  const typing = e.target.matches?.('input, textarea, select, [contenteditable], .xterm-helper-textarea');
  if (overlaid() && c !== 'c+k') return;
  const map = {
    'c+k': () => palette(),
    'c+n': () => import('./views/chats.js').then((m) => m.newChat()),
    'cs+n': () => newProject(),
    'c+b': () => toggleRail(),
    'c+,': () => go('#/settings'),
    'a+arrowleft': () => history.back(),
    'a+arrowright': () => history.forward(),
    '+f11': () => (isDesktop ? win('fullscreen') : document.fullscreenElement ? document.exitFullscreen() : document.documentElement.requestFullscreen?.()),
    'c+=': () => setZoom(zoom + 0.1),
    'c++': () => setZoom(zoom + 0.1),
    'cs++': () => setZoom(zoom + 0.1),
    'c+-': () => setZoom(zoom - 0.1),
    'c+0': () => setZoom(1),
    'cs+i': () => isDesktop && native('devtools'),
  };
  NAV.forEach(([path, , , k], i) => i < 6 && (map[k] = () => go('#/' + path)));
  if (!typing && c === 's+?') return (e.preventDefault(), shortcuts());
  if (!typing && c === '+?') return (e.preventDefault(), shortcuts());
  const fn = map[c];
  if (fn) {
    e.preventDefault();
    fn();
  }
}
function setZoom(z) {
  zoom = Math.max(0.6, Math.min(2, Math.round(z * 10) / 10));
  if (isDesktop) native('zoom', { factor: zoom });
  else document.body.style.zoom = zoom;
  toast(`Zoom ${Math.round(zoom * 100)}%`, { ms: 1200 });
}

// ------------------------------------------------------------ start
async function start() {
  tb = titlebar();
  initTopo();
  // links inside content open in the person's browser, never inside the app window
  document.addEventListener('click', (e) => {
    const a = e.target.closest('a[href]');
    if (!a || a.dataset.ext === '1') return;
    const href = a.getAttribute('href');
    if (/^(https?:|mailto:)/.test(href)) {
      e.preventDefault();
      openExternal(href);
    }
  });
  document.addEventListener('keydown', keys);
  onUnauthorized(() => {
    if (parseRoute().view !== 'pair') go('#/pair');
  });
  listen((ev) => {
    if (ev.kind === 'toast') toast(ev.data?.text || '', { err: ev.data?.err });
  });
  try {
    await load();
  } catch (e) {
    if (e.status === 401) {
      side();
      addEventListener('hashchange', render);
      if (parseRoute().view !== 'pair') location.hash = '#/pair';
      return render();
    }
    $('#view').replaceChildren(h('div', { class: 'v' }, errorBox(e, () => location.reload())));
    return;
  }
  connect();
  side();
  // a phone can install the app to its home screen (needs https or localhost; harmless elsewhere)
  if (!isDesktop && 'serviceWorker' in navigator) navigator.serviceWorker.register('/sw.js').catch(() => {});
  if (!isPop) import('./statusbar.js').then((m) => m.initStatusBar()).catch((e) => console.error(e));
  addEventListener('hashchange', render);
  render();
}

window.addEventListener('error', (e) => console.error(e.error || e.message));
start();

export { stamp };
