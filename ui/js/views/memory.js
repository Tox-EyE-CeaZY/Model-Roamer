// Memory for this installation, with shared project and global scopes and a private collaborator scope.
// Also serves a joined project's Memory tab (host project/global memory via proxy + local private notes).

import { h, ago, bytes, debounce } from '../util.js';
import { icon } from '../icons.js';
import { get, post, del, listen } from '../api.js';
import { S } from '../store.js';
import { dialog, toast, showMenu } from '../ui.js';
import { md, wireMd } from '../md.js';
import { openExternal } from '../native.js';
import { ic, empty, loading, errorBox, field, input, textarea, select, kicker, seg } from '../common.js';

const SCOPE_NAMES = { collaborator: 'Private', project: 'Project', global: 'Global' };

export default async function memory(el, r, opts = {}) {
  const collab = opts.collab || null;
  let scope = opts.project || r?.q?.project || '';
  const fixed = !!opts.project;
  const list = h('div', { class: 'mem-list' });
  const pane = h('div', { class: 'mem-pane' });
  const banner = h('div', { class: 'collab-mem-banner', hidden: true });
  const q = input('', { placeholder: 'Search memory', type: 'search' });
  let filter = '';

  const access = () => (collab ? collab.getAccess() : null);

  const projectSel = fixed ? null : select([['', 'Across projects'], ...S.projects.filter((p) => !p.id.startsWith('lnk_')).map((p) => [p.id, p.name])], scope, (v) => ((scope = v), load()), { 'aria-label': 'Memory project' });
  const scopeWrap = h('span', { class: 'mem-scope-wrap' });
  const toolbarActs = h('span', { class: 'row' });

  function drawScopeSelect() {
    const allowed = collab ? access().memoryScopes : Object.keys(SCOPE_NAMES);
    if (filter && !allowed.includes(filter)) {
      filter = '';
      openKey = '';
      pane.replaceChildren();
    }
    const sel = select(
      [['', 'All scopes'], ...allowed.map((k) => [k, SCOPE_NAMES[k]])],
      filter,
      (v) => {
        filter = v;
        openKey = '';
        pane.replaceChildren();
        load();
      },
      { 'aria-label': 'Memory scope' },
    );
    scopeWrap.replaceChildren(sel);
  }

  function drawToolbarActs() {
    const acc = access();
    const canNew = !collab || !acc?.revoked;
    const newLabel = collab && !acc?.canEditProjectMemory ? 'New private note' : 'New memory';
    toolbarActs.replaceChildren(
      canNew ? h('button', { class: 'btn sm primary', onclick: () => edit(null) }, ic('plus', 13), newLabel) : null,
      h('button', { class: 'btn sm', title: collab ? 'Recently deleted private notes' : 'Recently deleted', onclick: () => trash() }, ic('trash', 13)),
    );
  }

  drawScopeSelect();
  drawToolbarActs();

  if (!fixed) {
    el.append(h('div', { class: 'page-h' }, h('div', {}, h('h1', {}, 'Memory'), h('p', { class: 'lede' }, 'Facts the agents keep between threads: how the project works, your preferences, decisions made. Edit anything; the agents read it on their next turn.'))));
  }
  el.append(
    banner,
    h('div', { class: 'toolbar' }, q, projectSel, scopeWrap, toolbarActs),
    h('div', { class: 'mem' }, list, pane),
  );

  let items = [];
  let openKey = r?.q?.open || '';
  let alive = true;

  const keyOf = (m) => (collab ? `${m._origin}:${m._origin === 'host' ? `${m.scope}:${m.name}` : m.id}` : m.id);

  async function load() {
    if (!alive) return;
    if (!collab) {
      try {
        const qs = new URLSearchParams();
        if (scope) qs.set('project', scope);
        if (filter) qs.set('scope', filter);
        if (q.value.trim()) qs.set('q', q.value.trim());
        items = await get('memory?' + qs);
        drawList();
        if (openKey) show(openKey);
        else if (!pane.childElementCount) pane.replaceChildren(h('div', { class: 'mem-hint' }, ic('memory', 26), h('p', {}, 'Pick an entry to read it, or make one.')));
      } catch (e) {
        list.replaceChildren(errorBox(e, load));
      }
      return;
    }

    // Joined project mode: combine local private notes + host project (and host global if permitted).
    const acc = access();
    if (acc.revoked) {
      items = [];
      list.replaceChildren(empty('lock', 'Project memory unavailable', 'You no longer have access to this project.'));
      pane.replaceChildren();
      return;
    }
    const query = q.value.trim();
    const wantLocal = !filter || filter === 'collaborator';
    const wantHost = !filter || filter === 'project' || (filter === 'global' && acc.canReadGlobalMemory);
    let hostErr = '';
    const [localRows, hostRows] = await Promise.all([
      wantLocal
        ? get('memory?' + new URLSearchParams({ scope: 'collaborator', ...(query ? { q: query } : {}) })).catch(() => [])
        : Promise.resolve([]),
      wantHost
        ? get(collab.base + 'memory' + (() => {
          const qs = new URLSearchParams();
          if (filter === 'project' || filter === 'global') qs.set('scope', filter);
          if (query) qs.set('q', query);
          const str = qs.toString();
          return str ? '?' + str : '';
        })()).catch((e) => {
          hostErr = e.message || 'Host unreachable';
          return [];
        })
        : Promise.resolve([]),
    ]);
    if (!alive) return;
    banner.hidden = !hostErr;
    if (hostErr) {
      banner.replaceChildren(h('div', { class: 'collab-warn' }, ic('warn', 14), h('span', {}, `Shared host memory unavailable right now (${hostErr}). Your private notes below still work.`)));
    } else {
      banner.replaceChildren();
    }
    const localNorm = (Array.isArray(localRows) ? localRows : []).map((m) => ({ ...m, scope: 'collaborator', _origin: 'local' }));
    const hostNorm = (Array.isArray(hostRows) ? hostRows : [])
      .filter((m) => !m.deleted_at)
      .filter((m) => m.scope !== 'global' || acc.canReadGlobalMemory)
      .map((m) => ({ ...m, scope: m.scope || (m.project_id ? 'project' : 'global'), _origin: 'host' }));
    items = [...localNorm, ...hostNorm];
    drawList();
    if (openKey && items.some((m) => keyOf(m) === openKey)) {
      show(openKey);
    } else {
      openKey = '';
      if (!pane.childElementCount) {
        pane.replaceChildren(h('div', { class: 'mem-hint' }, ic('memory', 26), h('p', {}, 'Pick an entry to read it, or make one.')));
      }
    }
  }

  function drawList() {
    if (!items.length) {
      return list.replaceChildren(empty('memory', q.value ? 'Nothing matches' : 'No memory yet', q.value ? '' : 'Leads write memory as they learn about the project. You can add entries too: conventions, preferences, where things live.'));
    }
    const allowed = collab ? access().memoryScopes : Object.keys(SCOPE_NAMES);
    const groups = allowed.map((key) => [SCOPE_NAMES[key], items.filter((m) => m.scope === key)]);
    list.replaceChildren(...groups.filter(([, g]) => g.length).map(([label, g]) => h('div', {},
      kicker(label, h('b', { class: 'count' }, g.length)),
      g.map((m) => {
        const k = keyOf(m);
        return h('button', { class: 'mem-i' + (k === openKey ? ' on' : ''), onclick: () => show(k) },
          h('b', {}, m.name),
          h('small', {}, m.description || m.snippet || ''),
          h('span', { class: 'mono faint' }, (m.updated_by || '') + ' · ' + ago(m.updated_at)));
      }))));
  }

  async function show(key) {
    openKey = key;
    drawList();
    pane.replaceChildren(loading());
    try {
      let m;
      const item = items.find((x) => keyOf(x) === key);
      if (collab && item?._origin === 'host') {
        const full = await get(collab.base + 'memory/' + encodeURIComponent(item.name) + '?scope=' + encodeURIComponent(item.scope));
        m = { ...full, scope: full.scope || item.scope, _origin: 'host' };
      } else {
        const id = item?.id || key;
        const full = await get('memory/' + encodeURIComponent(id));
        m = { ...full, _origin: 'local' };
      }
      if (!alive || openKey !== key) return;
      const acc = access();
      const canEdit = !collab || m._origin === 'local' || (m.scope === 'project' && acc.canEditProjectMemory);
      const canDelete = !collab || m._origin === 'local';
      pane.replaceChildren(
        h('div', { class: 'mem-h' },
          h('div', {}, h('h2', {}, m.name), h('p', { class: 'faint' }, m.description)),
          h('span', { class: 'grow' }),
          canEdit ? h('button', { class: 'btn sm', onclick: () => edit(m) }, ic('edit', 13), 'Edit') : null,
          canDelete ? h('button', { class: 'ibtn', title: 'Delete', onclick: () => remove(m), html: icon('trash', 15) }) : null),
        h('div', { class: 'mono faint small' }, `${SCOPE_NAMES[m.scope] || m.scope} · ${m.updated_by || 'unknown'} · ${new Date(m.updated_at).toLocaleString()} · ${bytes((m.body || '').length)}`),
        wireMd(h('div', { class: 'md doc' }, ...[...h('div', { html: md(m.body || '_(empty)_') }).childNodes]), openExternal),
      );
    } catch (e) {
      if (alive) pane.replaceChildren(errorBox(e, () => show(key)));
    }
  }

  async function edit(m) {
    const acc = access();
    const editingHost = collab && m?._origin === 'host';
    const name = input(m?.name || '', { placeholder: 'short-kebab-name', disabled: !!editingHost });
    const desc = input(m?.description || '', { placeholder: 'One line: what this is, so agents know when to read it' });
    const body = textarea(m?.body || '', { rows: 14, class: 'input mono' });
    let scopeChoices;
    let initialScope;
    if (collab) {
      scopeChoices = [
        ['collaborator', 'Private (this installation)'],
        ...(acc.canEditProjectMemory ? [['project', 'This project (shared on host)']] : []),
      ];
      initialScope = m
        ? (m._origin === 'host' ? 'project' : 'collaborator')
        : (filter === 'project' && acc.canEditProjectMemory ? 'project' : acc.canEditProjectMemory ? 'project' : 'collaborator');
    } else {
      scopeChoices = [
        ['collaborator', 'Private (this installation)'],
        ['global', 'Global (every project)'],
        ...(fixed ? [[scope, 'This project']] : S.projects.filter((p) => !p.id.startsWith('lnk_')).map((p) => [p.id, p.name])),
      ];
      initialScope = m ? m.project_id || m.scope : filter === 'collaborator' || filter === 'global' ? filter : scope || 'global';
    }
    const where = select(scopeChoices, initialScope, null, { disabled: !!editingHost });
    let saved = null;
    let savedOrigin = 'local';
    const ok = await dialog({
      title: m ? 'Edit memory' : 'New memory',
      wide: true,
      body: h('div', { class: 'dlg-fields' }, h('div', { class: 'row2' }, field('Name', name), field('Scope', where)), field('Description', desc), field('Body (Markdown)', body)),
      ok: 'Save',
      check: async () => {
        if (!name.value.trim()) return 'Give it a name.';
        try {
          if (collab && where.value === 'project') {
            saved = await post(collab.base + 'memory', { name: name.value.trim(), description: desc.value, body: body.value, scope: 'project' });
            savedOrigin = 'host';
          } else {
            const acrossProjects = where.value === 'collaborator' || where.value === 'global';
            saved = await post('memory', {
              id: m?._origin === 'host' ? undefined : m?.id,
              name: name.value.trim(),
              description: desc.value,
              body: body.value,
              scope: acrossProjects ? where.value : 'project',
              project_id: acrossProjects ? null : where.value,
            });
            savedOrigin = 'local';
          }
        } catch (e) {
          return e.message;
        }
      },
    });
    if (ok && saved) {
      openKey = collab ? `${savedOrigin}:${savedOrigin === 'host' ? `project:${saved.name}` : saved.id}` : saved.id;
      toast(savedOrigin === 'host' ? 'Saved to the shared project’s memory' : 'Saved');
      load();
    }
  }

  async function remove(m) {
    await del('memory/' + m.id);
    openKey = '';
    pane.replaceChildren();
    load();
    toast(`Deleted ${m.name}`, { action: 'Undo', ms: 10000, run: () => post(`memory/${m.id}/restore`).then(() => ((openKey = collab ? `local:${m.id}` : m.id), load())) });
  }

  async function trash() {
    const qs = collab
      ? new URLSearchParams({ deleted: '1', scope: 'collaborator' })
      : new URLSearchParams({ deleted: '1', project: scope, scope: filter });
    const rows = await get('memory?' + qs).catch(() => []);
    showMenu(rows.length ? [{ header: 'Recently deleted' }, ...rows.map((m) => ({ label: m.name, icon: 'undo', kbd: ago(m.deleted_at), run: () => post(`memory/${m.id}/restore`).then(() => ((openKey = collab ? `local:${m.id}` : m.id), load(), toast('Restored'))) }))] : [{ header: 'Nothing deleted' }], innerWidth / 2, 160);
  }

  q.oninput = debounce(load, 200);
  list.replaceChildren(loading());
  await load();
  const unlisten = listen((ev) => (ev.kind === 'memory' || ev.kind === 'resync') && load());
  const unlive = collab?.onLiveChange?.(({ permsChanged, memoryChanged }) => {
    if (permsChanged) {
      drawScopeSelect();
      drawToolbarActs();
      load();
    } else if (memoryChanged) {
      load();
    }
  });
  return () => {
    alive = false;
    unlisten();
    unlive?.();
  };
}

export { seg };
