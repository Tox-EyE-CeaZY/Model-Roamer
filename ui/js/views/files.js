// A project's Files tab: the folder the agents work in, its files, and which of them an agent is working on right
// now (claimed, changed in a running thread's worktree, or just modified in the shared folder).

import { h } from '../util.js';
import { icon } from '../icons.js';
import { get, post } from '../api.js';
import { S, agentName, agentHue } from '../store.js';
import { go, openThread } from '../app.js';
import { toast } from '../ui.js';
import { ic, empty, loading, errorBox, timeAgo, repoSelect } from '../common.js';

const HOW = { claimed: 'claimed', changed: 'editing', recent: 'just changed' };
const size = (n) => (n < 1024 ? n + ' B' : n < 1048576 ? (n / 1024).toFixed(1) + ' KB' : (n / 1048576).toFixed(1) + ' MB');

export default async function filesTab(el, p, r, opts = {}) {
  const apiBase = opts.collab ? opts.collab.viewBase : `projects/${p.id}/`;
  let path = r?.q?.p || '';
  // which of the project's repositories (?repo=), when it has several
  const repo = r?.q?.repo && r.q.repo !== 'primary' ? r.q.repo : '';
  const at = (rel) => `#/p/${p.id}/files` + ((rel || repo) ? '?' + new URLSearchParams({ ...(rel ? { p: rel } : {}), ...(repo ? { repo } : {}) }) : '');
  const rq = repo ? '&repo=' + encodeURIComponent(repo) : '';
  let sel = null, data = null, dead = false;
  const bar = h('div', { class: 'fl-bar' });
  const live = h('div', { class: 'fl-live' });
  const list = h('div', { class: 'fl-list' });
  const view = h('aside', { class: 'fl-view', hidden: true });
  el.append(h('div', { class: 'fl' }, bar, live, h('div', { class: 'fl-split' }, list, view)));
  list.replaceChildren(loading());

  const chip = (m) => h('button', {
    class: 'fl-mark ' + m.how, style: { '--ah': agentHue(m.agent) },
    title: `${agentName(m.agent)} · ${m.title}${m.note ? ' — ' + m.note : ''} (${HOW[m.how] || m.how})`,
    onclick: (e) => (e.stopPropagation(), openThread(m.thread_id)),
  }, agentName(m.agent), h('small', {}, HOW[m.how] || m.how));

  function drawBar() {
    const parts = path ? path.split('/') : [];
    const crumb = (label, to) => h('button', { class: 'linkbtn', onclick: () => go(at(to)) }, label);
    const pick = repoSelect(p.repos, repo, (v) => go(`#/p/${p.id}/files` + (v && v !== 'primary' ? '?repo=' + encodeURIComponent(v) : '')));
    bar.replaceChildren(
      ...(pick ? [pick] : []),
      ic('folder', 14),
      crumb(data?.root || 'Project folder', ''),
      ...parts.flatMap((seg, i) => [h('span', { class: 'faint' }, '/'), crumb(seg, parts.slice(0, i + 1).join('/'))]),
      h('span', { class: 'grow' }),
      S.me?.local && data ? h('button', { class: 'btn sm', onclick: () => post('fs/reveal', { path: data.root + (path ? '/' + path : '') }).catch((e) => toast(e.message, { err: true })) }, ic('external', 12), 'Show in folder') : null);
  }

  function drawLive() {
    const a = data?.active || [];
    live.replaceChildren(...(a.length
      ? [h('b', { class: 'eyebrow' }, 'Working here now'), ...a.map((w) => h('button', { class: 'fl-mark running', style: { '--ah': agentHue(w.agent) }, title: w.title, onclick: () => openThread(w.thread_id) },
        agentName(w.agent), h('small', {}, w.title + (w.how === 'worktree' ? ` · ${w.files} changed` : ' · in the shared folder'))))]
      : [h('span', { class: 'faint small' }, data?.claims ? `${data.claims} claimed file${data.claims === 1 ? '' : 's'}; nobody is running right now.` : 'No agent is working in this folder right now.')]));
  }

  function row(e) {
    return h('div', {
      class: 'fl-row' + (e.marks.length ? ' hot' : '') + (sel === e.path ? ' on' : ''), tabindex: '0',
      onclick: () => (e.dir ? go(at(e.path)) : open(e.path)),
      onkeydown: (ev) => ev.key === 'Enter' && (e.dir ? go(at(e.path)) : open(e.path)),
    },
    h('span', { class: 'fl-ico', html: icon(e.dir ? 'folder' : 'file', 15) }),
    h('span', { class: 'fl-name' }, e.name + (e.dir ? '/' : '')),
    h('span', { class: 'fl-marks' }, e.marks.map(chip)),
    h('span', { class: 'fl-meta mono' }, e.dir ? '' : size(e.size)),
    h('span', { class: 'fl-meta mono' }, e.mtime ? timeAgo(e.mtime) : ''));
  }

  function drawList() {
    if (!data) return;
    const up = data.parent !== null ? h('div', { class: 'fl-row up', tabindex: '0', onclick: () => go(at(data.parent)) }, h('span', { class: 'fl-ico', html: icon('back', 15) }), h('span', { class: 'fl-name' }, '..')) : null;
    list.replaceChildren(...(data.entries.length ? [up, ...data.entries.map(row), data.truncated ? h('p', { class: 'faint small' }, 'Showing the first 2000 entries.') : null] : [up, empty('folder', 'Empty folder', 'Nothing here yet.')]).filter(Boolean));
  }

  async function open(rel) {
    sel = rel;
    drawList();
    view.hidden = false;
    view.replaceChildren(loading());
    try {
      const f = await get(`${apiBase}file?path=${encodeURIComponent(rel)}${rq}`);
      view.replaceChildren(
        h('div', { class: 'fl-view-h' }, h('b', { class: 'mono' }, rel), h('span', { class: 'grow' }), h('small', { class: 'faint mono' }, size(f.size)),
          h('button', { class: 'ibtn sm', title: 'Close', onclick: () => ((sel = null), (view.hidden = true), drawList()), html: icon('close', 14) })),
        f.binary ? h('p', { class: 'faint small' }, 'A binary file; it can’t be shown here.') : h('pre', { class: 'fl-code mono' }, f.text),
        f.truncated ? h('p', { class: 'faint small' }, 'Only the first 256 KB is shown.') : null);
    } catch (e) {
      view.replaceChildren(errorBox(e, () => open(rel)));
    }
  }

  async function load() {
    try {
      data = await get(`${apiBase}files?path=${encodeURIComponent(path)}${rq}`);
      if (dead) return;
      drawBar();
      drawLive();
      drawList();
    } catch (e) {
      if (!dead) list.replaceChildren(errorBox(e, load));
    }
  }
  await load();
  const timer = setInterval(() => !document.hidden && load(), 5000);
  return () => ((dead = true), clearInterval(timer));
}
