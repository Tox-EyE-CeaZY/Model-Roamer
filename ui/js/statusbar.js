// The status bar along the bottom of the main window: what is going on right now, one item at a time, plus the
// connection and anything that needs the person. Pop-out windows don't get one.

import { get, listen, onSocket } from './api.js';
import { S, watch, agentName } from './store.js';
import { h, debounce, dur, plural } from './util.js';
import { ic } from './common.js';
import { attention, needsYou, setActivity, watchAttention } from './attention.js';

const CYCLE_MS = 4500;

let data = { threads: [], decisions: [], terms: [], agents: [], update: null, quotas: [] };
let items = [];
let cur = 0;
let hold = false;
let shownKey = '';
let el, ticker, count, right;
let live = true;

/** What the bar can say, most pressing first. Each item knows where clicking it goes. */
function collect() {
  const out = [];
  const threadGo = (t) => () => import('./app.js').then((m) => (t.kind === 'lead' && t.project_id ? m.go('#/p/' + t.project_id) : m.openThread(t.id)));
  const where = (t) => (t.project ? t.project + ' · ' : '');
  for (const d of data.decisions) {
    out.push({ key: 'd' + d.id, tone: 'hot', icon: 'question', label: 'Needs you', text: d.question, sub: d.title, go: threadGo({ id: d.thread_id, kind: d.kind, project_id: d.project_id }) });
  }
  // threads that need the person: the same rule (and the same "seen" marks) as Home and the board
  const need = attention(data.threads).need;
  for (const t of need.filter((t) => t.status === 'blocked')) {
    out.push({ key: 'b' + t.id, tone: 'hot', icon: 'flag', label: 'Needs you', text: where(t) + t.title, sub: t.status_note, go: threadGo(t) });
  }
  for (const t of need.filter((t) => needsYou(t) === 'unread')) {
    out.push({ key: 'r' + t.id, tone: 'hot', icon: 'chat', label: 'New reply', text: (t.project || 'The lead') + ': the lead replied', go: threadGo(t) });
  }
  for (const a of data.agents.filter((a) => a.state === 'signin')) {
    out.push({ key: 'a' + a.id, tone: 'hot', icon: 'login', label: 'Sign in', text: `${a.name || a.id} isn't signed in`, go: () => import('./app.js').then((m) => m.go('#/agents')) });
  }
  for (const t of data.terms) {
    out.push({ key: 'x' + t.id, tone: 'live', icon: 'terminal', label: 'Running', text: t.label, since: t.started, go: () => import('./term.js').then((m) => m.showTerm(t.id, { title: t.label })) });
  }
  for (const t of data.threads.filter((t) => t.status === 'working')) {
    out.push({ key: 'w' + t.id, tone: 'live', icon: 'bolt', label: agentName(t.agent), text: where(t) + t.title, sub: t.status_note, since: t.started, go: threadGo(t) });
  }
  for (const t of data.threads.filter((t) => t.status === 'queued')) {
    out.push({ key: 'q' + t.id, tone: 'dim', icon: 'list', label: 'Queued', text: where(t) + t.title, sub: agentName(t.agent), go: threadGo(t) });
  }
  for (const a of data.agents.filter((a) => a.state === 'checking')) {
    out.push({ key: 'c' + a.id, tone: 'dim', icon: 'refresh', label: 'Checking', text: `${a.name || a.id}: sign-in and models`, go: () => import('./app.js').then((m) => m.go('#/agents')) });
  }
  for (const t of need.filter((t) => t.status === 'limited')) {
    out.push({ key: 'l' + t.id, tone: 'hot', icon: 'pause', label: 'Waiting on limit', text: where(t) + t.title, sub: t.status_note, go: threadGo(t) });
  }
  for (const a of data.agents.filter((a) => a.state === 'limit')) {
    out.push({ key: 'a' + a.id, tone: 'hot', icon: 'warn', label: 'Usage limit', text: `${a.name || a.id} is out of usage`, sub: a.note, go: () => import('./app.js').then((m) => m.go('#/usage')) });
  }
  for (const t of need.filter((t) => t.status === 'failed')) {
    out.push({ key: 'f' + t.id, tone: 'hot', icon: 'warn', label: 'Failed', text: where(t) + t.title, sub: t.status_note, go: threadGo(t) });
  }
  if (data.update) {
    out.push({ key: 'u', tone: 'acc', icon: 'download', label: 'Update', text: `${data.update} is ready. Restart to use it`, go: () => import('./app.js').then((m) => m.go('#/settings/about')) });
  }
  if (!live) out.unshift({ key: 'off', tone: 'hot', icon: 'wifi', label: 'Offline', text: 'Lost the connection. Reconnecting…' });
  return out;
}

function drawItem() {
  const it = items[cur];
  if (!it) {
    shownKey = '';
    const p = S.projects.length;
    ticker.replaceChildren(h('span', { class: 'sb-it quiet' }, ic('check', 12), h('b', {}, 'All quiet'), h('span', { class: 'sb-t' }, p ? `${plural(p, 'project')}, nothing running` : 'Nothing running')));
    count.hidden = true;
    return;
  }
  const fresh = it.key !== shownKey;
  shownKey = it.key;
  const node = h('button', { class: `sb-it ${it.tone}${fresh ? ' in' : ''}`, type: 'button', title: [it.label, it.text, it.sub].filter(Boolean).join(' · '), onclick: () => it.go?.() },
    ic(it.icon, 12), h('b', {}, it.label), h('span', { class: 'sb-t' }, it.text), it.sub ? h('span', { class: 'sb-sub' }, it.sub) : null,
    it.since ? h('span', { class: 'sb-time' }, dur(Date.now() - it.since)) : null);
  ticker.replaceChildren(node);
  count.hidden = items.length < 2;
  count.textContent = `${cur + 1}/${items.length}`;
}

function drawRight() {
  const working = data.threads.filter((t) => t.status === 'working').length + data.terms.length;
  const needs = attention(data.threads, data.decisions).count;
  const remote = !S.me.local;
  right.replaceChildren(
    h('div', { class: 'sb-meters' }, ...(data.quotas || []).map(meter)),
    needs ? h('button', { class: 'sb-seg hot', type: 'button', title: 'Jump to what needs you', onclick: () => jumpTo('hot') }, ic('flag', 12), String(needs)) : null,
    h('button', { class: 'sb-seg' + (working ? ' live' : ''), type: 'button', title: working ? `${plural(working, 'thing')} running. Click to see the next` : 'Nothing running', onclick: () => jumpTo('live') }, ic('bolt', 12), String(working)),
    h('span', { class: 'sb-seg' + (live ? '' : ' hot'), title: live ? 'Live updates are connected' : 'Reconnecting…' },
      ic(remote ? 'server' : 'desktop', 12), remote ? S.server.name || 'Server' : 'This computer', h('i', { class: 'sb-dot' + (live ? '' : ' off') })),
    h('a', { class: 'sb-seg dim', href: '#/settings/about', title: 'About and updates' }, 'v' + (S.version || '')),
  );
  document.documentElement.classList.toggle('busy', working > 0);
}

// ---------------------------------------------------------- plan usage meters
const pct = (w) => (w && typeof w.used === 'number' ? Math.round(w.used * 100) : null);
const resetIn = (ts) => (ts ? `resets ${new Date(ts).toLocaleString(undefined, { weekday: 'short', hour: 'numeric', minute: '2-digit' })} (in ${dur(ts - Date.now())})` : '');

/** One harness: its 5-hour use as a bar in its own colour; hover for the 5-hour and weekly detail. */
function meter(q) {
  const five = pct(q.five_hour);
  const limited = q.limit?.active;
  const fill = limited ? 100 : five ?? 0;
  const el = h('button', { class: 'sb-meter' + (five == null && !limited ? ' unknown' : '') + (limited || fill >= 90 ? ' hot' : ''), type: 'button', style: { '--ah': q.hue ?? 200 },
    onclick: () => import('./app.js').then((m) => m.go('#/usage')),
    onmouseenter: () => showPop(el, q), onmouseleave: hidePop, onfocus: () => showPop(el, q), onblur: hidePop },
    h('span', { class: 'sb-m-name' }, q.name.split(' ')[0]),
    h('span', { class: 'sb-m-bar' }, h('i', { style: { width: fill + '%' } })),
    h('span', { class: 'sb-m-pct' }, limited ? 'OUT' : five == null ? '–' : five + '%'));
  return el;
}

let pop = null;
function hidePop() {
  pop?.remove();
  pop = null;
}
function showPop(anchor, q) {
  hidePop();
  const row = (label, w) => {
    const p = pct(w);
    return h('div', { class: 'sb-p-row' },
      h('span', {}, label),
      h('span', { class: 'sb-m-bar wide' }, h('i', { style: { width: (p ?? 0) + '%' } })),
      h('b', {}, p == null ? 'unknown' : p + '%'),
      w?.resets_at ? h('small', {}, resetIn(w.resets_at)) : null);
  };
  const lim = q.limit?.active ? h('div', { class: 'sb-p-hot' }, `Out of usage${q.limit.until ? ', ' + resetIn(q.limit.until) : ''}. ${q.limit.note || ''}`) : null;
  const unknown = pct(q.five_hour) == null && pct(q.weekly) == null;
  pop = h('div', { class: 'sb-pop cut', style: { '--ah': q.hue ?? 200 } },
    h('div', { class: 'sb-p-h' }, h('i', { class: 'sb-p-dot' }), h('b', {}, q.name)),
    lim,
    row('5-hour', q.five_hour),
    row('Weekly', q.weekly),
    h('div', { class: 'sb-p-foot' },
      `${plural(q.turns_5h || 0, 'turn')} in the last 5 hours, ${q.turns_7d || 0} this week`,
      q.at ? ` · checked ${dur(Date.now() - q.at)} ago` : '',
      !unknown ? '' : '. Not read yet: it is checked at start-up, every ten minutes and after each turn (sign in under Agents if it stays empty).'));
  document.body.append(pop);
  const r = anchor.getBoundingClientRect();
  const w = pop.offsetWidth;
  pop.style.left = Math.max(8, Math.min(innerWidth - w - 8, r.left + r.width / 2 - w / 2)) + 'px';
  pop.style.bottom = innerHeight - r.top + 6 + 'px';
}

function jumpTo(tone) {
  const i = items.findIndex((x, n) => x.tone === tone && n > cur);
  const j = i >= 0 ? i : items.findIndex((x) => x.tone === tone);
  if (j >= 0) {
    cur = j;
    drawItem();
  }
}

function rebuild() {
  const key = items[cur]?.key;
  items = collect();
  const keep = items.findIndex((x) => x.key === key);
  cur = keep >= 0 ? keep : Math.min(cur, Math.max(0, items.length - 1));
  drawItem();
  drawRight();
}

const refresh = debounce(async () => {
  try {
    data = await get('activity', { quiet: true, timeout: 15000 });
  } catch { /* the next event or tick tries again */ }
  setActivity(data); // redraws through watchAttention, and the side rail's count with it
}, 300);

export function initStatusBar() {
  ticker = h('div', { class: 'sb-ticker' });
  count = h('button', { class: 'sb-count', type: 'button', title: 'Next', onclick: () => step(1) });
  right = h('div', { class: 'sb-right' });
  el = h('footer', { id: 'statusbar', onmouseenter: () => (hold = true), onmouseleave: () => (hold = false) },
    h('button', { class: 'sb-nav', type: 'button', title: 'Previous', onclick: () => step(-1) }, ic('left', 11)),
    ticker, count,
    h('button', { class: 'sb-nav', type: 'button', title: 'Next', onclick: () => step(1) }, ic('right', 11)),
    right);
  document.body.append(el);
  document.documentElement.classList.add('has-sb');
  let last = Date.now();
  const step = (d) => {
    last = Date.now();
    if (!items.length) return;
    cur = (cur + d + items.length) % items.length;
    drawItem();
  };
  setInterval(() => {
    if (!hold && items.length > 1 && Date.now() - last >= CYCLE_MS) {
      step(1);
    } else if (items[cur]?.since) drawItem(); // the running clock
  }, 1000);
  // a slow safety net; events do the real work
  setInterval(refresh, 30000);
  listen((ev) => {
    switch (ev.kind) {
      case 'term':
        // output arrives as term events too: only a new terminal or one ending changes the picture
        if (ev.data?.exit !== undefined || !data.terms.some((t) => t.id === ev.data?.id)) refresh();
        break;
      case 'progress': {
        const t = data.threads.find((x) => x.id === ev.thread);
        if (t) {
          t.status_note = ev.data?.text || '';
          rebuild();
        }
        break;
      }
      case 'quota': case 'turn': case 'running': case 'thread': case 'decision': case 'agent': case 'limits': case 'setup': case 'update': case 'project': case 'resync':
        refresh();
        break;
    }
  });
  onSocket((ok) => {
    live = ok;
    rebuild();
    if (ok) refresh();
  });
  watch((what) => (what === 'all' || what === 'projects') && drawRight());
  watchAttention(rebuild); // marked seen elsewhere, or new activity (setActivity) arrived
  refresh();
}
