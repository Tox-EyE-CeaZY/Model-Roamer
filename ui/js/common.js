// Pieces every screen uses: agent stamps, status chips, form fields, empty/loading/error states, the folder picker.

import { h, ago } from './util.js';
import { icon } from './icons.js';
import { agent, agentName, agentHue, S } from './store.js';
import { get, post } from './api.js';
import { dialog, toast } from './ui.js';

export const ic = (name, size = 16) => h('i', { class: 'ico', html: icon(name, size) });

/** The skewed route chip that names an agent, in its own colour. */
export function stamp(id, { model, small, lead } = {}) {
  const a = agent(id);
  const label = a.info?.name || id;
  const m = model ? (a.info?.models || []).find((x) => x.id === model)?.label || model : '';
  return h('span', { class: 'stamp' + (small ? ' sm' : '') + (lead ? ' lead' : ''), style: { '--ah': agentHue(id) }, title: label + (m ? ' · ' + m : '') + (lead ? ' (lead)' : '') },
    h('b', {}, lead ? '★ ' + label : label), m ? h('em', {}, m) : null);
}

const STATUS = {
  idle: ['Idle', 'faint'],
  queued: ['Queued', 'dim'],
  working: ['Working', 'live'],
  waiting: ['Waiting', 'dim'],
  blocked: ['Needs you', 'hot'],
  review: ['Finished', 'dim'], // a worker's report to the lead, not a call for the person (see attention.js)
  done: ['Done', 'acc'],
  failed: ['Failed', 'hot'],
  paused: ['Paused', 'dim'],
  cancelled: ['Stopped', 'faint'],
  limited: ['Usage limit', 'hot'],
};
export const statusLabel = (s) => (STATUS[s] || [s])[0];
export function statusChip(t) {
  const st = typeof t === 'string' ? t : t.running ? 'working' : t.status;
  const [label, tone] = STATUS[st] || [st, 'dim'];
  return h('span', { class: 'status ' + tone }, st === 'working' ? h('i', { class: 'pulse' }) : null, t.merged ? 'Merged' : t.resolved && st !== 'working' ? 'Resolved' : label);
}

export const kicker = (text, extra) => h('div', { class: 'kicker' }, h('span', {}, text), extra || null);

export function field(label, input, hint) {
  return h('label', { class: 'field' }, h('span', { class: 'f-l' }, label), input, hint ? h('small', { class: 'f-h' }, hint) : null);
}

export function input(value = '', props = {}) {
  return h('input', { class: 'input', value: value ?? '', spellcheck: 'false', ...props });
}

export function textarea(value = '', props = {}) {
  const t = h('textarea', { class: 'input', rows: 4, spellcheck: 'true', ...props });
  t.value = value ?? '';
  return t;
}

export function select(options, value, onchange, props = {}) {
  const s = h('select', { class: 'input sel', ...props, onchange: onchange ? (e) => onchange(e.target.value) : null },
    options.map((o) => (Array.isArray(o) ? o : [o, o])).map(([v, l]) => h('option', { value: v, selected: String(v) === String(value ?? '') }, l)));
  return s;
}

export function toggle(on, onchange, { label, title } = {}) {
  const b = h('button', { class: 'tgl' + (on ? ' on' : ''), type: 'button', role: 'switch', 'aria-checked': String(!!on), title: title || label || null }, h('i'));
  b.onclick = () => {
    const v = !b.classList.contains('on');
    b.classList.toggle('on', v);
    b.setAttribute('aria-checked', String(v));
    onchange?.(v);
  };
  return label ? h('div', { class: 'tgl-row' }, h('span', {}, label), b) : b;
}

export function seg(options, value, onchange) {
  const el = h('div', { class: 'seg' });
  const draw = (v) =>
    el.replaceChildren(...options.map(([k, l]) => h('button', { class: 'seg-b' + (String(k) === String(v) ? ' on' : ''), type: 'button', onclick: () => (draw(k), onchange(k)) }, l)));
  draw(value);
  return el;
}

export function empty(iconName, title, text, action) {
  return h('div', { class: 'empty' }, h('div', { class: 'empty-ic', html: icon(iconName, 30) }), h('h3', {}, title), text ? h('p', {}, text) : null, action || null);
}

export const loading = (text = 'Loading') => h('div', { class: 'loading' }, h('span', { class: 'hatch' }), h('span', {}, text));

export function errorBox(e, retry) {
  return h('div', { class: 'errbox' }, ic('warn', 18), h('div', {}, h('b', {}, "That didn't work"), h('p', {}, e?.message || String(e))), retry ? h('button', { class: 'btn', onclick: retry }, 'Try again') : null);
}

/** Model choices for an agent (with "CLI default" first), as select options. */
export function modelOptions(id, { withDefault = true } = {}) {
  if (String(id).startsWith('buddy:')) return [['', 'Each uses its own model']];
  const ms = agent(id).info?.models || [];
  const opts = ms.map((m) => [m.id, m.label + (m.note ? ' — ' + m.note : '')]);
  return withDefault && !ms.some((m) => m.id === '') ? [['', 'CLI default'], ...opts] : opts;
}

export function agentOptions({ leadOnly, auto, buddies = true } = {}) {
  const list = Object.values(S.agents).filter((a) => !leadOnly || a.info?.can_lead !== false);
  const opts = list.map((a) => [a.id, a.info?.name + (a.installed === false ? ' (not installed)' : a.auth?.signed_in === 'no' ? ' (not signed in)' : a.limit?.active ? ' (at its limit)' : '')]);
  // buddy pairs: two agents that take turns, picked like one agent
  const pairs = leadOnly || !buddies ? [] : (S.settings.buddies || []).map((b) => ['buddy:' + b.id, '⇄ ' + b.name]);
  return auto ? [['', 'Let the router choose'], ...opts, ...pairs] : [...opts, ...pairs];
}

/** How hard a model thinks. The levels come from the CLI's model list when it gives them (they differ per model),
 * else a common set; "" means the agent's own setting. Agents without a thinking setting get no options. */
const EFFORT = { minimal: 'Minimal', low: 'Low', medium: 'Medium', high: 'High', xhigh: 'Extra high', max: 'Max', ultra: 'Ultra' };
export function effortOptions(agentId, model, defaultLabel = 'Default thinking') {
  if (!['claude', 'codex', 'antigravity'].includes(agentId)) return null;
  const ms = agent(agentId).info?.models || [];
  const m = ms.find((x) => x.id === model);
  const lv = m?.efforts?.length ? m.efforts : [...new Set(ms.flatMap((x) => x.efforts || []))];
  const list = lv.length ? lv : agentId === 'claude' ? ['low', 'medium', 'high', 'xhigh', 'max'] : ['low', 'medium', 'high'];
  return [['', defaultLabel], ...list.map((e) => [e, EFFORT[e] || e])];
}

/** The thinking-strength select for a routing row. It follows the row's agent: call `set(agentId)` when that changes. */
export function routeEffort(agentId, value, onchange) {
  const el = h('span');
  let cur = value || '';
  const set = (a) => {
    const o = effortOptions(a, agent(a).config?.model || '', 'Default thinking');
    if (!o || !o.some(([v]) => v === cur)) cur = '';
    el.replaceChildren(...(o ? [select(o, cur, (v) => onchange((cur = v)))] : []));
  };
  set(agentId);
  return { el, set };
}

/** An agent + model (+ thinking strength) set of selects that keep each other in step. */
export function agentModelPicker({ agentId, model, effort = '', leadOnly, auto, onchange }) {
  let a = agentId, m = model, e = effort;
  const mSel = h('span'), eSel = h('span');
  const drawEffort = () => {
    const o = a ? effortOptions(a, m) : null;
    if (!o || !o.some(([v]) => v === e)) e = '';
    eSel.replaceChildren(...(o ? [select(o, e, (v) => ((e = v), onchange?.(a, m, e)))] : []));
  };
  const drawModels = () => {
    if (!a) return mSel.replaceChildren();
    mSel.replaceChildren(select(modelOptions(a), m, (v) => ((m = v), drawEffort(), onchange?.(a, m, e))));
  };
  const aSel = select(agentOptions({ leadOnly, auto }), a, (v) => {
    a = v;
    m = v && !v.startsWith('buddy:') ? agent(v).config?.model ?? '' : '';
    e = '';
    drawModels();
    drawEffort();
    onchange?.(a, m, e);
  });
  drawModels();
  drawEffort();
  return { el: h('div', { class: 'pick2' }, aSel, mSel, eSel), get: () => ({ agent: a, model: m, effort: e }) };
}

export function timeAgo(ts) {
  return h('time', { class: 'ago', title: ts ? new Date(ts).toLocaleString() : '', 'data-ts': ts || '' }, ts ? ago(ts) : '');
}
// relative times stay fresh
setInterval(() => document.querySelectorAll('time.ago[data-ts]').forEach((t) => t.dataset.ts && (t.textContent = ago(+t.dataset.ts))), 30000);

/** Browse folders on the machine the app runs on (the server, when connected to one). Resolves to a path or null. */
export async function pickFolder({ title = 'Choose a folder', start = '' } = {}) {
  let cur = start, showHidden = false, chosen = start;
  const list = h('div', { class: 'fp-list' });
  const pathIn = input(start, { placeholder: 'Type or paste a path' });
  const crumbs = h('div', { class: 'fp-top' });
  async function go(p) {
    list.replaceChildren(loading());
    try {
      const r = await get('fs?path=' + encodeURIComponent(p || ''));
      cur = r.path;
      chosen = r.path;
      pathIn.value = r.path;
      const rows = [];
      if (!r.path) rows.push(...r.roots.map((x) => h('button', { class: 'fp-i', type: 'button', onclick: () => go(x.path) }, ic('folder'), h('span', {}, x.name), h('small', {}, x.path))));
      else {
        if (r.parent) rows.push(h('button', { class: 'fp-i up', type: 'button', onclick: () => go(r.parent) }, ic('up'), h('span', {}, '..')));
        for (const e of r.entries.filter((e) => e.dir && (showHidden || !e.hidden)))
          rows.push(h('button', { class: 'fp-i', type: 'button', ondblclick: () => go(r.path + (r.path.endsWith(r.sep) ? '' : r.sep) + e.name), onclick: () => go(r.path + (r.path.endsWith(r.sep) ? '' : r.sep) + e.name) }, ic(e.git ? 'branch' : 'folder'), h('span', {}, e.name), e.git ? h('small', { class: 'acc' }, 'git') : null));
        if (rows.length <= 1) rows.push(h('div', { class: 'fp-none' }, 'No folders in here.'));
      }
      list.replaceChildren(...rows);
      crumbs.replaceChildren(h('button', { class: 'chipbtn', type: 'button', onclick: () => go('') }, 'Places'), r.git ? h('span', { class: 'status acc' }, 'Git repository') : null);
    } catch (e) {
      list.replaceChildren(errorBox(e, () => go(cur)));
    }
  }
  pathIn.onkeydown = (e) => e.key === 'Enter' && (e.preventDefault(), e.stopPropagation(), go(pathIn.value));
  const newBtn = h('button', { class: 'chipbtn', type: 'button', onclick: async () => {
    if (!cur) return toast('Open a folder first, then make one inside it.');
    const n = await askLine('New folder', '', 'Folder name');
    if (!n) return;
    try {
      const sep = cur.includes('\\') ? '\\' : '/';
      const r = await post('fs/mkdir', { path: cur + (cur.endsWith(sep) ? '' : sep) + n });
      go(r.path);
    } catch (e) {
      toast(e.message, { err: true });
    }
  } }, '+ New folder');
  const hid = toggle(false, (v) => ((showHidden = v), go(cur)), { label: 'Hidden' });
  go(start);
  const ok = await dialog({ title, wide: true, cls: 'fp', body: h('div', { class: 'fp-body' }, h('div', { class: 'fp-row' }, pathIn, newBtn, hid), crumbs, list), ok: 'Use this folder' });
  return ok ? (pathIn.value.trim() || chosen) : null;
}

/** One line of text in a dialog. */
export async function askLine(title, value = '', label = '', { ok = 'OK', placeholder = '' } = {}) {
  const i = input(value, { placeholder });
  const res = await dialog({ title, body: field(label, i), ok });
  return res ? i.value.trim() : null;
}

/** A thread row (used by the board, chats, search). */
export function threadLine(t, { onopen, showProject } = {}) {
  return h('button', { class: 'tline' + (t.unread ? ' unread' : ''), type: 'button', onclick: onopen },
    h('span', { class: 'tl-t' }, t.title),
    showProject && t.project_name ? h('small', {}, t.project_name) : null,
    stamp(t.agent, { small: true }),
    statusChip(t));
}

export function copyable(text, { mono = true } = {}) {
  return h('div', { class: 'copyable' + (mono ? ' mono' : '') }, h('code', {}, text), h('button', { class: 'ibtn sm', type: 'button', title: 'Copy', onclick: (e) => {
    navigator.clipboard?.writeText(text).then(() => toast('Copied')).catch(() => {});
    e.stopPropagation();
  }, html: icon('copy', 14) }));
}

export const agentNameOf = agentName;

/** A project's repository as one short line: owner/repo, or the folder's name. */
export const repoLabel = (r) => (r ? r.name + (r.primary ? ' (primary)' : '') : '');

/** Pick one of a project's repositories (from GET projects/:id/repos). Null when there is only one to pick. */
export function repoSelect(repos, value, onchange, props = {}) {
  if (!repos || repos.length < 2) return null;
  return select(repos.map((r) => [r.id, repoLabel(r)]), value || 'primary', onchange, props);
}

/** A folder path on the machine the app runs on, with Browse and a live check (exists, readable, git or not).
 *  `get()` gives the typed path; `ok()` resolves true when the last check passed. */
export function folderInput({ value = '', placeholder = 'Full path, e.g. /srv/code/thing or C:\code\thing', allowMissing = false, onchange } = {}) {
  const inp = input(value, { placeholder });
  const note = h('small', { class: 'f-h fcheck' });
  let last = null, timer = 0, seq = 0;
  async function check() {
    const v = inp.value.trim();
    const n = ++seq;
    if (!v) return ((last = null), note.replaceChildren(), (note.className = 'f-h fcheck'), null);
    note.className = 'f-h fcheck';
    note.replaceChildren('Checking…');
    try {
      const r = await get('fs/check?path=' + encodeURIComponent(v));
      if (n !== seq) return last;
      last = r;
      if (r.ok) {
        const f = r.folder;
        note.className = 'f-h fcheck ok';
        note.replaceChildren(ic(f.git ? 'branch' : 'folder', 12), f.git ? `Git repository${f.branch ? ' on ' + f.branch : ''}${f.remote ? ' · ' + f.remote.replace(/^https?:\/\/(www\.)?github\.com\//, '').replace(/\.git$/, '') : ''}` : 'A plain folder (no git): threads work in it directly, without branches.');
      } else {
        note.className = 'f-h fcheck ' + (allowMissing && /doesn.t exist/.test(r.error) ? '' : 'bad');
        note.replaceChildren(r.error);
      }
      onchange?.(r);
      return r;
    } catch (e) {
      if (n === seq) note.replaceChildren(e.message);
      return null;
    }
  }
  inp.oninput = () => (clearTimeout(timer), (timer = setTimeout(check, 350)));
  const browse = h('button', { class: 'btn', type: 'button', onclick: async () => {
    const p = await pickFolder({ start: inp.value.trim() });
    if (p) ((inp.value = p), check());
  } }, ic('folder', 13), 'Browse');
  if (value) check();
  return {
    el: h('div', {}, h('div', { class: 'row' }, inp, browse), note),
    input: inp,
    get: () => inp.value.trim(),
    set: (v) => ((inp.value = v), check()),
    check,
    ok: async () => (await check())?.ok === true,
    last: () => last,
  };
}
