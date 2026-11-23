// Memory: what the agents (and you) keep for later. Global memory is shared by every project; project memory
// belongs to one. Every agent sees the index and reads the entries it needs, so keep them short and specific.

import { h, ago, bytes, debounce } from '../util.js';
import { icon } from '../icons.js';
import { get, post, del, listen } from '../api.js';
import { S } from '../store.js';
import { dialog, toast, showMenu } from '../ui.js';
import { md, wireMd } from '../md.js';
import { openExternal } from '../native.js';
import { ic, empty, loading, errorBox, field, input, textarea, select, kicker, seg } from '../common.js';

export default async function memory(el, r, opts = {}) {
  let scope = opts.project || r?.q?.project || '';
  const fixed = !!opts.project;
  const list = h('div', { class: 'mem-list' });
  const pane = h('div', { class: 'mem-pane' });
  const q = input('', { placeholder: 'Search memory', type: 'search' });
  const scopeSel = fixed ? null : select([['', 'Global (every project)'], ...S.projects.map((p) => [p.id, p.name])], scope, (v) => ((scope = v), load()));
  if (!fixed)
    el.append(h('div', { class: 'page-h' }, h('div', {}, h('h1', {}, 'Memory'), h('p', { class: 'lede' }, 'Facts the agents keep between threads: how the project works, your preferences, decisions made. Edit anything; the agents read it on their next turn.'))));
  el.append(
    h('div', { class: 'toolbar' }, q, scopeSel, h('button', { class: 'btn sm primary', onclick: () => edit(null) }, ic('plus', 13), 'New memory'), h('button', { class: 'btn sm', title: 'Recently deleted', onclick: () => trash() }, ic('trash', 13))),
    h('div', { class: 'mem' }, list, pane));
  let items = [], openId = r?.q?.open || '';

  async function load() {
    try {
      const qs = new URLSearchParams();
      if (scope) qs.set('project', scope);
      if (q.value.trim()) qs.set('q', q.value.trim());
      items = await get('memory?' + qs);
      drawList();
      if (openId) show(openId);
      else if (!pane.childElementCount) pane.replaceChildren(h('div', { class: 'mem-hint' }, ic('memory', 26), h('p', {}, 'Pick an entry to read it, or make one. ', scope ? 'This list shows the project’s memory and the global memory every project shares.' : 'Global memory is shared by every project.')));
    } catch (e) {
      list.replaceChildren(errorBox(e, load));
    }
  }
  function drawList() {
    if (!items.length) return list.replaceChildren(empty('memory', q.value ? 'Nothing matches' : 'No memory yet', q.value ? '' : 'Leads write memory as they learn about the project. You can add entries too: conventions, preferences, where things live.'));
    const groups = [[scope ? 'This project' : 'Global', items.filter((m) => (m.project_id || '') === scope)], ...(scope ? [['Global', items.filter((m) => !m.project_id)]] : [])];
    list.replaceChildren(...groups.filter(([, g]) => g.length).map(([label, g]) => h('div', {}, kicker(label, h('b', { class: 'count' }, g.length)), g.map((m) => h('button', { class: 'mem-i' + (m.id === openId ? ' on' : ''), onclick: () => show(m.id) },
      h('b', {}, m.name), h('small', {}, m.description || ''), h('span', { class: 'mono faint' }, (m.updated_by || '') + ' · ' + ago(m.updated_at)))))));
  }
  async function show(id) {
    openId = id;
    drawList();
    pane.replaceChildren(loading());
    try {
      const m = await get('memory/' + id);
      pane.replaceChildren(
        h('div', { class: 'mem-h' }, h('div', {}, h('h2', {}, m.name), h('p', { class: 'faint' }, m.description)), h('span', { class: 'grow' }),
          h('button', { class: 'btn sm', onclick: () => edit(m) }, ic('edit', 13), 'Edit'),
          h('button', { class: 'ibtn', title: 'Delete', onclick: () => remove(m), html: icon('trash', 15) })),
        h('div', { class: 'mono faint small' }, `${m.project_id ? 'Project' : 'Global'} · ${m.updated_by || 'unknown'} · ${new Date(m.updated_at).toLocaleString()} · ${bytes((m.body || '').length)}`),
        wireMd(h('div', { class: 'md doc' }, ...[...h('div', { html: md(m.body || '_(empty)_') }).childNodes]), openExternal));
    } catch (e) {
      pane.replaceChildren(errorBox(e));
    }
  }
  async function edit(m) {
    const name = input(m?.name || '', { placeholder: 'short-kebab-name' });
    const desc = input(m?.description || '', { placeholder: 'One line: what this is, so agents know when to read it' });
    const body = textarea(m?.body || '', { rows: 14, class: 'input mono' });
    const where = fixed ? null : select([['', 'Global (every project)'], ...S.projects.map((p) => [p.id, p.name])], m ? m.project_id || '' : scope);
    let saved = null;
    const ok = await dialog({
      title: m ? 'Edit memory' : 'New memory',
      wide: true,
      body: h('div', { class: 'dlg-fields' }, h('div', { class: 'row2' }, field('Name', name), where ? field('Belongs to', where) : null), field('Description', desc), field('Body (Markdown)', body)),
      ok: 'Save',
      check: async () => {
        if (!name.value.trim()) return 'Give it a name.';
        try {
          saved = await post('memory', { id: m?.id, name: name.value, description: desc.value, body: body.value, project_id: fixed ? scope : where.value || null });
        } catch (e) {
          return e.message;
        }
      },
    });
    if (ok && saved) {
      openId = saved.id;
      toast('Saved');
      load();
    }
  }
  async function remove(m) {
    await del('memory/' + m.id);
    openId = '';
    pane.replaceChildren();
    load();
    toast(`Deleted ${m.name}`, { action: 'Undo', ms: 10000, run: () => post(`memory/${m.id}/restore`).then(() => ((openId = m.id), load())) });
  }
  async function trash() {
    const rows = await get('memory?deleted=1' + (scope ? '&project=' + scope : '')).catch(() => []);
    showMenu(rows.length ? [{ header: 'Recently deleted' }, ...rows.map((m) => ({ label: m.name, icon: 'undo', kbd: ago(m.deleted_at), run: () => post(`memory/${m.id}/restore`).then(() => ((openId = m.id), load(), toast('Restored'))) }))] : [{ header: 'Nothing deleted' }], innerWidth / 2, 160);
  }
  q.oninput = debounce(load, 200);
  list.replaceChildren(loading());
  await load();
  return listen((ev) => (ev.kind === 'memory' || ev.kind === 'resync') && load());
}

export { seg };
