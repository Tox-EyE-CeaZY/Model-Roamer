// A project: the chat with its lead on the left, its threads in lanes on the right, plus git, memory, artifacts
// and settings tabs.

import { h, ago } from '../util.js';
import { icon } from '../icons.js';
import { get, post, patch, del, listen } from '../api.js';
import { S, watch, agentName, agentHue, project as projectOf, addRef } from '../store.js';
import { go, back, openThread, deleteProject } from '../app.js';
import { popOut, setTitle, openExternal } from '../native.js';
import { dialog, toast, showMenu } from '../ui.js';
import { mountThread, decisionCard } from './thread.js';
import { ic, stamp, statusChip, field, input, textarea, select, seg, agentModelPicker, routeEffort, agentOptions, modelOptions, pickFolder, folderInput, askLine, repoSelect, repoLabel, loading, errorBox, empty, timeAgo, kicker, copyable } from '../common.js';
import { needsYou, isFinished } from '../attention.js';
import { scrollStrip } from '../scrollstrip.js';

const TABS = [['', 'Board', 'board'], ['files', 'Files', 'folder'], ['git', 'Git', 'branch'], ['memory', 'Memory', 'memory'], ['artifacts', 'Artifacts', 'artifact'], ['settings', 'Settings', 'settings']];
const PANE_KEY = (id) => 'mr-proj-pane-' + id;

function savedPane(id) {
  try {
    const v = localStorage.getItem(PANE_KEY(id));
    if (v === 'lead' || v === 'threads') return v;
  } catch { /* private window */ }
  return 'lead';
}

function rememberPane(id, pane) {
  try {
    localStorage.setItem(PANE_KEY(id), pane);
  } catch { /* private window */ }
}

export default async function projectView(el, r, ctx) {
  let p;
  try {
    p = await get('projects/' + r.id);
  } catch (e) {
    if (e.status === 404 || /no such/i.test(e.message)) return el.replaceChildren(empty('board', 'That project isn’t here', 'It may have been deleted.', h('a', { class: 'btn', href: '#/' }, 'Home')));
    throw e;
  }
  setTitle(`${p.name} · Model Roamer`);
  const tab = r.tab || '';
  // a project joined from someone else's Model Roamer: its page is the joined view, with the person's own chat
  if (p.id.startsWith('lnk_') && !tab) return r.q.t ? go('#/t/' + r.q.t) : go('#/settings/join?link=' + encodeURIComponent(p.id));
  const headEl = h('div', { class: 'p-head', style: { '--ph': p.color } });
  const body = h('div', { class: 'p-body' });
  el.append(headEl, body);
  el.style.setProperty('--ph', p.color);

  let mobilePane = savedPane(p.id);
  let onPaneChange = null;
  let threadBadgeState = null;
  let tabStrip = null;
  let leadTabEl = null;
  let threadsTabEl = null;
  let threadsBadgeHost = null;

  function renderThreadBadge() {
    if (!threadsBadgeHost) return;
    const sp = projectOf(p.id);
    const needsYou = threadBadgeState ? threadBadgeState.needsYou : (p.decisions?.length ?? sp?.decisions ?? 0);
    const unread = threadBadgeState ? threadBadgeState.unread : (sp?.unread ?? 0);
    const extraUnread = threadBadgeState ? threadBadgeState.extraUnread : 0;
    const nodes = [];
    if (needsYou > 0) {
      nodes.push(h('b', {
        class: 'p-tab-badge hot',
        title: `${needsYou} need you` + (unread ? ` · ${unread} unread` : ''),
      }, needsYou));
      if (extraUnread > 0) nodes.push(h('i', { class: 'p-tab-dot', title: `${unread} unread` }));
    } else if (unread > 0) {
      nodes.push(h('b', { class: 'p-tab-badge acc', title: `${unread} unread` }, unread));
    }
    threadsBadgeHost.replaceChildren(...nodes);
    tabStrip?.update();
  }

  function syncMobileTabHighlight() {
    leadTabEl?.classList.toggle('on', tab === '' && mobilePane === 'lead');
    threadsTabEl?.classList.toggle('on', tab === '' && mobilePane === 'threads');
    tabStrip?.update();
  }

  function pickMobilePane(next, e) {
    mobilePane = next;
    rememberPane(p.id, next);
    if (tab === '' && onPaneChange) {
      e?.preventDefault();
      syncMobileTabHighlight();
      onPaneChange(next);
      tabStrip?.revealActive({ smooth: true });
    }
  }

  function drawHead() {
    tabStrip?.destroy();
    const where = p.repo_url ? p.repo_url.replace(/^https?:\/\/(www\.)?github\.com\//, '').replace(/\.git$/, '') : p.folder || 'Kept by Model Roamer';
    threadsBadgeHost = h('span', { class: 'p-tab-badges' });
    leadTabEl = h('a', {
      class: 'p-tab p-tab-mob' + (tab === '' && mobilePane === 'lead' ? ' on' : ''),
      href: '#/p/' + p.id,
      onclick: (e) => pickMobilePane('lead', e),
    }, ic('chat', 14), 'Lead');
    threadsTabEl = h('a', {
      class: 'p-tab p-tab-mob' + (tab === '' && mobilePane === 'threads' ? ' on' : ''),
      href: '#/p/' + p.id,
      onclick: (e) => pickMobilePane('threads', e),
    }, ic('board', 14), 'Threads', threadsBadgeHost);

    const nav = h('nav', { class: 'p-tabs', 'aria-label': 'Project sections' },
      leadTabEl,
      threadsTabEl,
      ...TABS.map(([k, label, ico]) => h('a', {
        class: 'p-tab' + (k === '' ? ' p-tab-desk' : '') + (k === tab ? ' on' : ''),
        href: '#/p/' + p.id + (k ? '/' + k : ''),
      }, ic(ico, 14), label)));

    tabStrip = scrollStrip(nav);

    headEl.replaceChildren(
      h('div', { class: 'p-title' },
        h('i', { class: 'p-sw' }),
        h('h1', {}, p.name),
        h('span', { class: 'p-sub mono', title: p.home }, ic(p.repo_url ? 'branch' : 'folder', 13), h('span', {}, where), p.base_branch ? [h('span', { class: 'faint' }, '·'), h('span', {}, p.base_branch)] : null,
          p.repos?.length > 1 ? h('a', { class: 'chipbtn', href: '#/p/' + p.id + '/settings', title: p.repos.map(repoLabel).join(', ') }, `+${p.repos.length - 1} more`) : null),
        h('span', { class: 'grow' }),
        h('div', { class: 'p-acts' },
          h('button', { class: 'btn primary sm', title: 'New thread', 'aria-label': 'New thread', onclick: () => newThreadDialog(p.id) }, ic('plus', 13), h('span', { class: 'p-new-lbl' }, 'New thread')),
          h('button', { class: 'ibtn', title: 'More', 'aria-label': 'More project actions', onclick: (e) => {
            const b = e.currentTarget.getBoundingClientRect();
            showMenu([
              { label: 'New thread…', icon: 'plus', run: () => newThreadDialog(p.id) },
              { label: 'Pop the project chat out', icon: 'popout', run: () => popOut('#/t/' + p.lead.id, { key: 't:' + p.lead.id }) },
              p.home && S.me.local ? { label: 'Show the folder', icon: 'folder', run: () => post('fs/reveal', { path: p.home }) } : null,
              p.repo_url ? { label: 'Open on GitHub', icon: 'external', run: () => openExternal(p.repo_url.replace(/\.git$/, '')) } : null,
              { sep: true },
              { label: 'Delete project', icon: 'trash', danger: true, run: () => deleteProject(p) },
            ], b.right, b.bottom + 4, { anchor: e.currentTarget });
          }, html: icon('more', 16) }))),
      tabStrip.el);
    renderThreadBadge();
  }
  drawHead();

  const unlisten = listen((ev) => {
    if (ev.kind === 'project' && ev.project === p.id && !ev.data?.deleted) {
      p = { ...p, ...ev.data };
      drawHead();
    }
    if (ev.kind === 'repos' && ev.project === p.id) get(`projects/${p.id}/repos`).then((l) => ((p.repos = l), drawHead())).catch(() => {});
  });
  const unwatch = watch((w) => {
    if (w === 'projects' || w === 'all') renderThreadBadge();
  });
  let inner = null;
  if (tab === '') {
    inner = await board(body, p, r, ctx, {
      getPane: () => mobilePane,
      onPane: (fn) => (onPaneChange = fn),
      onBadge: (info) => {
        threadBadgeState = info;
        renderThreadBadge();
      },
    });
  }
  else if (tab === 'files') inner = await (await import('./files.js')).default(body, p, r);
  else if (tab === 'git') inner = await gitTab(body, p, r.q?.repo || '');
  else if (tab === 'memory') inner = await (await import('./memory.js')).default(body, { q: {} }, { project: p.id });
  else if (tab === 'artifacts') inner = await (await import('./artifacts.js')).default(body, { q: {} }, { project: p.id });
  else if (tab === 'settings') inner = await settingsTab(body, p, (np) => ((p = { ...p, ...np }), drawHead()));
  return () => (unlisten(), unwatch(), tabStrip?.destroy(), inner?.());
}

// ------------------------------------------------------------ board
const LANES = [
  ['attn', 'Needs you', (t) => !!needsYou(t)], // one rule for every screen: attention.js
  ['work', 'Working', (t) => t.running || (!t.resolved && ['working', 'queued', 'waiting', 'idle'].includes(t.status))],
  ['fin', 'Finished', isFinished], // reported to the lead, which reviews it: folded and quiet
  ['held', 'Paused', (t) => !t.resolved && ['paused', 'cancelled'].includes(t.status) && !t.running],
  ['done', 'Done', (t) => !t.running && (t.resolved || t.status === 'done')],
];

async function board(el, p, r, ctx, mobile = {}) {
  const leadBox = h('div', { class: 'b-lead' });
  const lanes = h('div', { class: 'b-lanes' });
  const decisions = h('div', { class: 'b-decisions' });
  const claimsEl = h('div', { class: 'b-claims' });
  const boardEl = h('div', { class: 'b-right' }, decisions, lanes, claimsEl);
  const peekEl = h('div', { class: 'b-peek', hidden: true });
  const split = h('div', { class: 'board', dataset: { pane: mobile.getPane?.() || 'lead' } }, leadBox, boardEl, peekEl);
  el.append(split);
  let threads = [], doneOpen = false, finOpen = false;
  try {
    doneOpen = localStorage.getItem('mr-done-' + p.id) === '1';
    finOpen = localStorage.getItem('mr-fin-' + p.id) === '1';
  } catch { /* fine */ }

  const stopLead = await mountThread(leadBox, p.lead.id, { compact: true });
  leadBox.prepend(h('div', { class: 'b-lead-h' },
    h('span', { class: 'eyebrow' }, 'Project chat'),
    stamp(p.lead_agent, { model: p.lead_model, small: true, lead: true }),
    h('span', { class: 'grow' }),
    h('button', { class: 'ibtn sm', title: 'Open as a full thread', onclick: () => go('#/t/' + p.lead.id), html: icon('fullscreen', 14) }),
    h('button', { class: 'ibtn sm', title: 'Pop out', onclick: () => popOut('#/t/' + p.lead.id, { key: 't:' + p.lead.id }), html: icon('popout', 14) })));

  async function load() {
    try {
      threads = (await get(`projects/${p.id}/threads`)).filter((t) => t.kind !== 'lead');
      const full = await get('projects/' + p.id);
      p.decisions = full.decisions;
      p.claims = full.claims;
      draw();
    } catch (e) {
      lanes.replaceChildren(errorBox(e, load));
    }
  }

  function card(t) {
    const deps = (t.depends_on || '').split(',').filter(Boolean);
    const waitingOn = deps.filter((d) => !threads.find((x) => x.id === d && (x.status === 'done' || x.merged)));
    return h('div', {
      class: 'tcard cut' + (t.unread ? ' unread' : '') + (t.running ? ' running' : ''),
      style: { '--ah': agentHue(t.agent) },
      tabindex: '0',
      onclick: (e) => !e.target.closest('button') && peek(t.id),
      onkeydown: (e) => e.key === 'Enter' && peek(t.id),
      onauxclick: (e) => e.button === 1 && openThread(t.id, { pop: true }),
      oncontextmenu: (e) => (e.preventDefault(), cardMenu(t, e.clientX, e.clientY)),
    },
    h('div', { class: 'tc-top' }, stamp(t.agent, { model: t.model, small: true }), t.kind === 'review' ? h('span', { class: 'tag' }, 'review') : null, h('span', { class: 'grow' }), statusChip(t)),
    h('h4', {}, t.title),
    t.status_note ? h('p', { class: 'tc-note' }, t.status_note) : t.last_body ? h('p', { class: 'tc-last' }, t.last_body.replace(/[#*_`>]/g, '').slice(0, 160)) : null,
    h('div', { class: 'tc-foot mono' },
      t.repo_id && p.repos?.length > 1 ? h('span', { class: 'tc-repo', title: 'Repository' }, ic('folder', 11), p.repos.find((x) => x.id === t.repo_id)?.name || 'removed repo') : null,
      t.branch ? h('span', { title: t.branch }, ic(t.merged ? 'merge' : 'branch', 11), t.merged ? 'merged' : t.branch.replace(/^roamer\//, '')) : null,
      waitingOn.length ? h('span', { class: 'hot', title: 'Waits for other threads' }, ic('flag', 11), 'waits for ' + waitingOn.length) : null,
      h('span', { class: 'grow' }),
      timeAgo(t.updated_at),
      h('button', { class: 'ibtn xs', title: 'Pop out', onclick: () => openThread(t.id, { pop: true }), html: icon('popout', 12) })));
  }

  function cardMenu(t, x, y) {
    const a = (action) => post(`threads/${t.id}/action`, { action }).then(load).catch((e) => toast(e.message, { err: true }));
    showMenu([
      { label: 'Open beside the chat', icon: 'thread', run: () => peek(t.id) },
      { label: 'Open full view', icon: 'fullscreen', run: () => go('#/t/' + t.id) },
      { label: 'Open in its own window', icon: 'popout', run: () => openThread(t.id, { pop: true }) },
      { label: 'Reference in the lead chat', icon: 'chat', run: () => { addRef(p.lead.id, t); toast('Added to your next message to the lead'); } },
      { sep: true },
      t.running ? { label: 'Pause', icon: 'pause', run: () => a('pause') } : null,
      t.running ? { label: 'Stop', icon: 'stop', run: () => a('stop') } : null,
      ['paused', 'limited', 'cancelled'].includes(t.status) && !t.running ? { label: 'Resume', icon: 'play', run: () => a('resume') } : null,
      t.status === 'failed' ? { label: 'Retry', icon: 'refresh', run: () => a('retry') } : null,
      { label: t.resolved ? 'Reopen' : 'Resolve', icon: t.resolved ? 'undo' : 'check', run: () => a(t.resolved ? 'reopen' : 'resolve') },
      { sep: true },
      { label: 'Delete', icon: 'trash', danger: true, run: async () => {
        await del('threads/' + t.id);
        load();
        toast('Thread deleted', { action: 'Undo', ms: 9000, run: () => post(`threads/${t.id}/restore`).then(load) });
      } },
    ], x, y);
  }

  function draw() {
    decisions.replaceChildren(...(p.decisions || []).map((d) => h('div', { class: 'b-dec' },
      h('button', { class: 'linkbtn small', onclick: () => peek(d.thread_id) }, 'from ' + (d.thread_id === p.lead.id ? 'the lead' : threads.find((t) => t.id === d.thread_id)?.title || 'a thread')),
      decisionCard(d, (i) => post('decisions/' + d.id, { choice: i }).then(load).catch((e) => toast(e.message, { err: true }))))));
    if (!threads.length) {
      lanes.replaceChildren(empty('thread', 'No threads yet', 'Tell the lead what you want in the project chat. It opens threads for the right agents. You can also open one yourself.', h('button', { class: 'btn', onclick: () => newThreadDialog(p.id) }, ic('plus', 13), 'New thread')));
    } else {
      lanes.replaceChildren(...LANES.map(([k, label, test]) => {
        const list = threads.filter(test);
        if (!list.length && k !== 'work') return null;
        const folds = k === 'done' || k === 'fin';
        const collapsed = folds && !(k === 'done' ? doneOpen : finOpen);
        return h('section', { class: 'lane l-' + k },
          h('button', { class: 'lane-h', onclick: folds ? () => {
            if (k === 'fin') finOpen = !finOpen;
            else doneOpen = !doneOpen;
            try {
              localStorage.setItem(`mr-${k}-` + p.id, (k === 'fin' ? finOpen : doneOpen) ? '1' : '');
            } catch { /* fine */ }
            draw();
          } : null }, h('span', {}, label), h('b', { class: 'count' + (k === 'attn' ? ' hot' : '') }, list.length), folds ? h('i', { class: 'lane-chev' + (collapsed ? '' : ' open'), html: icon('chevDown', 12) }) : null),
          collapsed ? null : h('div', { class: 'lane-cards' }, list.length ? list.map(card) : h('p', { class: 'faint small lane-empty' }, 'Nothing running right now.')));
      }));
    }
    const claims = p.claims || [];
    claimsEl.replaceChildren(...(claims.length ? [kicker('Claimed files', h('small', { class: 'faint' }, 'so agents stay out of each other’s way')), h('div', { class: 'claims' }, claims.map((c) => h('span', { class: 'claim', style: { '--ah': agentHue(c.agent) }, title: `${c.title}${c.note ? ' — ' + c.note : ''}` }, h('code', {}, c.path), h('small', {}, agentName(c.agent)))))] : []));
    const attnList = threads.filter(LANES[0][2]);
    const openDecs = p.decisions || [];
    const extraDecs = openDecs.filter((d) => !attnList.some((t) => t.id === d.thread_id)).length;
    const unread = threads.filter((t) => t.unread).length;
    const extraUnread = threads.filter((t) => t.unread && !attnList.includes(t)).length;
    mobile.onBadge?.({ needsYou: attnList.length + extraDecs, unread, extraUnread });
  }

  // A thread opened from the board sits beside the project chat, in place of the board; the address carries it
  // (#/p/<id>?t=<thread>), so Back closes it and Expand's full view comes back here.
  const peek = (id) => go('#/p/' + p.id + '?t=' + id);
  let peeked = null, stopPeek = null;
  mobile.onPane?.((next) => {
    split.dataset.pane = next;
    if (peeked) go('#/p/' + p.id);
  });
  async function showPeek(id) {
    if (id === peeked) return;
    stopPeek?.();
    stopPeek = null;
    peeked = id;
    boardEl.hidden = !!id;
    peekEl.hidden = !id;
    split.classList.toggle('peeking', !!id);
    if (!id) return peekEl.replaceChildren();
    const title = h('b', { class: 'bp-title' }, 'Thread');
    const body = h('div', { class: 'bp-body' });
    peekEl.replaceChildren(h('div', { class: 'b-lead-h bp-h' },
      h('button', { class: 'ibtn sm', title: 'Back to the board (Alt+Left)', onclick: () => back('#/p/' + p.id), html: icon('back', 14) }),
      title,
      h('span', { class: 'grow' }),
      h('button', { class: 'ibtn sm', title: 'Expand to the full view', onclick: () => go('#/t/' + id), html: icon('fullscreen', 14) }),
      h('button', { class: 'ibtn sm', title: 'Pop out', onclick: () => popOut('#/t/' + id, { key: 't:' + id }), html: icon('popout', 14) }),
      h('button', { class: 'ibtn sm', title: 'Close', onclick: () => back('#/p/' + p.id), html: icon('close', 14) })), body);
    const stop = await mountThread(body, id, { compact: true, ctx: { setPopTitle: (t) => (title.textContent = t) } });
    if (peeked !== id) return stop?.();
    stopPeek = stop;
  }
  ctx?.onRoute?.((nr) => showPeek(nr.q.t || null));
  await showPeek(r.q?.t || null);

  await load();
  let timer = 0;
  const soon = () => (clearTimeout(timer), (timer = setTimeout(load, 300)));
  const unlisten = listen((ev) => {
    if (ev.kind === 'resync') return load();
    if (ev.project !== p.id && !(ev.kind === 'running' && threads.some((t) => t.id === ev.thread))) return;
    if (['thread', 'turn', 'running', 'decision', 'claims', 'message', 'progress'].includes(ev.kind)) soon();
  });
  return () => (unlisten(), stopPeek?.(), stopLead?.());
}

// ------------------------------------------------------------ new thread (by hand)
export async function newThreadDialog(projectId, { repo = '' } = {}) {
  const repos = await get(`projects/${projectId}/repos`).catch(() => []);
  let repoV = repo || 'primary';
  const repoPick = repoSelect(repos, repoV, (v) => (repoV = v));
  const title = input('', { placeholder: 'Short name for the job' });
  const instr = textarea('', { rows: 6, placeholder: 'What should be done, with enough context to do it without asking.' });
  const crit = textarea('', { rows: 3, placeholder: 'How we’ll know it’s done (tests pass, the page shows X…). Optional.' });
  const kinds = select([['', 'Choose for me'], ...S.taskKinds.map((k) => [k.id, k.label])], '');
  const pick = agentModelPicker({ agentId: '', model: '', auto: true });
  const ws = seg([['worktree', 'Own branch'], ['shared', 'Project folder'], ['scratch', 'Empty folder']], 'worktree', (v) => (wsv = v));
  let wsv = 'worktree';
  let made = null;
  const ok = await dialog({
    title: 'New thread',
    text: 'Opens a worker thread yourself. The lead hears about it and plans around it.',
    wide: true,
    body: h('div', { class: 'dlg-fields' }, field('Title', title), field('Instructions', instr), field('Done when', crit),
      h('div', { class: 'row2' }, field('Kind of work', kinds, 'Routing picks the agent for the kind.'), field('Agent', pick.el)),
      repoPick ? field('Repository', repoPick, 'Which of the project’s repositories this thread works on. Its branch, merge and pull request go there.') : null,
      field('Workspace', ws, 'Own branch: a git worktree, merged when you say so. Project folder: works in place (good for research and reading).')),
    ok: 'Start thread',
    check: async () => {
      if (!title.value.trim()) return 'Give it a title.';
      if (!instr.value.trim()) return 'Say what the agent should do.';
      const { agent: a, model: m, effort: ef } = pick.get();
      try {
        made = await post(`projects/${projectId}/workers`, { title: title.value.trim(), instructions: instr.value, criteria: crit.value, task_kind: kinds.value, agent: a, model: m, effort: ef, workspace: wsv, repo: repoV });
      } catch (e) {
        return e.message;
      }
    },
  });
  if (ok && made) toast(`Started ${made.title} with ${agentName(made.agent)}`, { action: 'Open', run: () => go('#/p/' + projectId + '?t=' + made.id) });
}

// ------------------------------------------------------------ git
async function gitTab(el, p, repo = '') {
  el.replaceChildren(loading('Reading the repository'));
  const rq = repo ? '?repo=' + encodeURIComponent(repo) : '';
  const [g, prs] = await Promise.all([get(`projects/${p.id}/git${rq}`).catch((e) => ({ error: e.message })), get(`projects/${p.id}/prs${rq}`).catch((e) => ({ error: e.message }))]);
  const pickRepo = repoSelect(p.repos, repo, (v) => go(`#/p/${p.id}/git` + (v && v !== 'primary' ? '?repo=' + encodeURIComponent(v) : '')));
  const top = pickRepo ? h('div', { class: 'repo-bar' }, h('span', { class: 'eyebrow' }, 'Repository'), pickRepo) : null;
  if (g.error) return el.replaceChildren(...[top, errorBox({ message: g.error })].filter(Boolean));
  if (!g.repo) {
    return el.replaceChildren(...[top].filter(Boolean), empty('branch', 'Not a git repository', g.git ? `${g.folder} has no git history. Agents still work there, but in place (no branches, no merging). Ask the lead to run “git init” if you want branches.` : 'Git isn’t installed on this machine. Install it under Agents › Tools.', g.git ? null : h('a', { class: 'btn', href: '#/agents?tab=tools' }, 'Install Git')));
  }
  const changes = (g.status || '').split('\n').filter((l) => l && !l.startsWith('##')); // "## branch" is a header, not a change
  el.replaceChildren(...[top].filter(Boolean), h('div', { class: 'git-grid' },
    h('section', { class: 'panel cut' }, kicker('Repository'),
      h('dl', { class: 'facts' },
        h('dt', {}, 'Folder'), h('dd', {}, copyable(g.folder)),
        h('dt', {}, 'Branch'), h('dd', { class: 'mono' }, g.branch || '—'),
        h('dt', {}, 'Remote'), h('dd', { class: 'mono' }, g.remote || 'none')),
      changes.length ? [kicker('Uncommitted', h('b', { class: 'count' }, changes.length)), h('pre', { class: 'mono small status-pre' }, changes.slice(0, 40).join('\n'))] : h('p', { class: 'faint small' }, 'Working tree clean.')),
    h('section', { class: 'panel cut' }, kicker('Recent commits'),
      h('div', { class: 'commits' }, (g.log || []).map((c) => h('div', { class: 'commit' }, h('code', {}, String(c.sha || '').slice(0, 7)), h('span', {}, c.subject), h('small', { class: 'faint' }, c.author + ' · ' + (c.when || '')))))),
    h('section', { class: 'panel cut wide' }, kicker('Pull requests', prs.repo ? h('a', { class: 'chipbtn', href: `https://github.com/${prs.repo}/pulls` }, 'On GitHub') : null),
      !prs.github ? h('p', { class: 'faint small' }, 'The remote isn’t on GitHub.')
        : prs.error ? h('p', { class: 'faint small' }, prs.error + ' — sign in under Settings › GitHub.')
          : prs.prs?.length ? h('div', { class: 'prs' }, prs.prs.map((r) => h('a', { class: 'pr-i', href: r.url }, ic('pr', 14), h('b', {}, '#' + r.number), h('span', {}, r.title), h('code', { class: 'faint' }, r.branch),
            r.draft ? h('span', { class: 'tag' }, 'draft') : r.merged ? h('span', { class: 'tag' }, 'merged') : r.state === 'closed' ? h('span', { class: 'tag' }, 'closed') : null,
            h('small', { class: 'faint' }, r.user),
            r.state === 'open' && !r.draft ? h('button', { class: 'btn xs', onclick: (e) => (e.preventDefault(), mergePr(p, r, () => gitTab(el, p, repo), repo)) }, 'Merge') : null)))
            : h('p', { class: 'faint small' }, 'No pull requests yet.'))));
}

async function mergePr(p, r, after, repo = '') {
  const method = select([['merge', 'Merge commit'], ['squash', 'Squash into one commit'], ['rebase', 'Rebase onto ' + (r.base || 'the base')]], 'merge');
  const okd = await dialog({ title: `Merge pull request #${r.number}?`, text: `Merges ${r.branch} into ${r.base} on GitHub. GitHub refuses if checks or reviews it requires haven't passed.`, body: field('How', method), ok: 'Merge' });
  if (!okd) return;
  try {
    await post(`projects/${p.id}/prs/${r.number}/merge`, { method: method.value, repo });
    toast(`Merged #${r.number}`, { action: 'View', run: () => openExternal(r.url) });
    after();
  } catch (e) {
    toast('Merge failed: ' + e.message, { err: true, ms: 9000 });
  }
}

// ------------------------------------------------------------ settings
async function settingsTab(el, p, onSaved) {
  const name = input(p.name);
  const folder = input(p.folder, { placeholder: 'Empty: Model Roamer keeps the files' });
  const repo = input(p.repo_url, { placeholder: 'https://github.com/you/repo' });
  const base = input(p.base_branch, { placeholder: 'main' });
  const instr = textarea(p.instructions, { rows: 8 });
  const lead = agentModelPicker({ agentId: p.lead_agent, model: p.lead_model, leadOnly: true });
  let color = p.color;
  const swatches = h('div', { class: 'swatches' }, [128, 38, 175, 205, 262, 318, 85, 0].map((c) => h('button', { class: 'sw' + (c === color ? ' on' : ''), type: 'button', style: { '--gh': c }, onclick: (e) => {
    color = c;
    swatches.querySelectorAll('.sw').forEach((s) => s.classList.remove('on'));
    e.currentTarget.classList.add('on');
  } })));
  const routing = { ...(p.routing || {}) };
  const routeRows = S.taskKinds.map((k) => {
    const rule = () => routing[k.id] || S.settings.routing?.[k.id] || 'claude';
    const ef = routeEffort(rule(), routing['effort:' + k.id] ?? S.settings.routing?.['effort:' + k.id], (v) => (routing['effort:' + k.id] = v));
    return h('div', { class: 'route-row r3' }, h('span', {}, k.label),
      select([['', `Default (${agentName(S.settings.routing?.[k.id] || 'claude')})`], ...agentOptions()], routing[k.id] || '', (v) => { if (v) routing[k.id] = v; else delete routing[k.id]; routing['effort:' + k.id] = ''; ef.set(rule()); }),
      ef.el);
  });
  const save = async () => {
    const { agent: la, model: lm } = lead.get();
    try {
      const np = await patch('projects/' + p.id, { name: name.value, folder: folder.value.trim(), repo_url: repo.value.trim(), base_branch: base.value.trim(), instructions: instr.value, lead_agent: la, lead_model: lm, color, routing });
      onSaved(np);
      toast('Saved');
    } catch (e) {
      toast(e.message, { err: true });
    }
  };
  el.replaceChildren(h('div', { class: 'p-settings' },
    h('section', { class: 'panel cut' }, kicker('Project'),
      field('Name', name),
      field('Colour', swatches),
      field('Folder', h('div', { class: 'row' }, folder, h('button', { class: 'btn', onclick: async () => {
        const f = await pickFolder({ start: folder.value });
        if (f) folder.value = f;
      } }, ic('folder', 13), 'Browse'))),
      field('Repository', repo, 'Used for pull requests. With no folder, it is cloned into Model Roamer’s data.'),
      field('Base branch', base, 'Workers branch from here and merge back into it.')),
    reposPanel(p),
    h('section', { class: 'panel cut' }, kicker('Lead'),
      field('Lead agent', lead.el, 'Switching the lead keeps the history: the new lead reads a recap on its next turn.'),
      field('Project instructions', instr, 'Every agent in the project reads these.')),
    h('section', { class: 'panel cut' }, kicker('Routing for this project', h('small', { class: 'faint' }, 'which agent gets which kind of work')), h('div', { class: 'routes' }, routeRows)),
    h('div', { class: 'save-bar' }, h('button', { class: 'btn primary', onclick: save }, 'Save changes'), h('span', { class: 'grow' }), h('button', { class: 'btn danger', onclick: () => deleteProject(p) }, ic('trash', 13), 'Delete project'))));
}

// ------------------------------------------------------------ repositories
// The project's repositories: the primary (its own folder / repository, set above) and any added beside it, each a
// GitHub repository Model Roamer clones or a folder on the machine it runs on. Threads choose which one they work on.
function reposPanel(p) {
  const box = h('section', { class: 'panel cut repos' });
  const here = S.server?.headless || !S.me?.local ? 'the server' : 'this computer';
  let repos = p.repos || [], adding = null, dead = false;

  async function load() {
    try {
      repos = await get(`projects/${p.id}/repos`);
      p.repos = repos;
      if (!dead) draw();
    } catch (e) {
      box.replaceChildren(kicker('Repositories'), errorBox(e, load));
    }
  }

  async function remove(r) {
    const okd = await dialog({
      title: `Remove ${r.name}?`,
      text: `Takes it off this project. Nothing on disk is deleted (${r.kind === 'github' ? 'the clone stays in Model Roamer’s data' : r.folder + ' stays as it is'}), and you can undo right after.`,
      ok: 'Remove', danger: true,
    });
    if (!okd) return;
    try {
      await del(`projects/${p.id}/repos/${r.id}`);
      await load();
      toast(`Removed ${r.name}`, { action: 'Undo', ms: 9000, run: () => post(`projects/${p.id}/repos/${r.id}/restore`).then(load).then(() => toast('Restored')).catch((e) => toast(e.message, { err: true })) });
    } catch (e) {
      toast(e.message, { err: true, ms: 9000 });
    }
  }

  async function rename(r) {
    const n = await askLine('Rename repository', r.name, 'The name agents and threads see');
    if (!n || n === r.name) return;
    try {
      await patch(`projects/${p.id}/repos/${r.id}`, { name: n });
      load();
    } catch (e) {
      toast(e.message, { err: true });
    }
  }

  function state(r) {
    if (r.clone_error) return h('span', { class: 'tag hot', title: r.clone_error }, 'clone failed');
    if (r.cloning) return h('span', { class: 'tag' }, 'cloning…');
    if (!r.present) return h('span', { class: 'tag hot', title: r.folder }, 'folder missing');
    return h('span', { class: 'tag' }, r.git ? 'git' : 'plain folder');
  }

  function rowOf(r) {
    return h('div', { class: 'repo-row' },
      h('span', { class: 'repo-ic', html: icon(r.kind === 'github' ? 'branch' : 'folder', 16) }),
      h('div', { class: 'repo-main' },
        h('b', {}, r.name, r.primary ? h('span', { class: 'tag acc' }, 'primary') : null, state(r)),
        h('small', { class: 'mono faint', title: r.home }, r.kind === 'github' && r.url ? r.url : r.home || r.folder),
        r.clone_error ? h('small', { class: 'f-err' }, r.clone_error) : h('small', { class: 'faint' }, [r.base_branch ? 'base ' + r.base_branch : null, r.threads ? `${r.threads} open thread${r.threads === 1 ? '' : 's'}` : null].filter(Boolean).join(' · '))),
      h('div', { class: 'row-btns' },
        r.clone_error ? h('button', { class: 'btn xs', onclick: () => patch(`projects/${p.id}/repos/${r.id}`, { retry_clone: true }).then(load).catch((e) => toast(e.message, { err: true })) }, ic('refresh', 11), 'Retry clone')
          : h('button', { class: 'btn xs', title: 'Open a thread on this repository', onclick: () => newThreadDialog(p.id, { repo: r.id }) }, ic('plus', 11), 'Thread'),
        r.primary ? null : h('button', { class: 'ibtn sm', title: 'Rename', onclick: () => rename(r), html: icon('edit', 14) }),
        r.primary ? null : h('button', { class: 'ibtn sm', title: r.threads ? 'Resolve its open threads first' : 'Remove from the project', onclick: () => remove(r), html: icon('trash', 14) })));
  }

  function addForm() {
    let kind = 'github';
    const url = input('', { placeholder: 'https://github.com/you/repo  or  you/repo' });
    const folder = folderInput();
    const name = input('', { placeholder: 'Optional: from the repository or folder' });
    const base = input('', { placeholder: 'Optional: its default or current branch' });
    const err = h('p', { class: 'f-err' });
    const where = h('div');
    const drawWhere = () => where.replaceChildren(kind === 'github'
      ? field('Repository', url, 'Model Roamer clones it into its own data. Sign in to GitHub in Settings for private repositories.')
      : field('Folder on ' + here, folder.el, 'A git repository gets a branch per thread; a plain folder is worked in directly.'));
    drawWhere();
    const submit = async (e) => {
      const btn = e.currentTarget;
      err.textContent = '';
      const b = { name: name.value.trim(), base_branch: base.value.trim() };
      if (kind === 'github') {
        if (!url.value.trim()) return (err.textContent = 'Paste the repository address.');
        b.url = url.value.trim();
      } else {
        if (!folder.get()) return (err.textContent = 'Choose a folder.');
        b.folder = folder.get();
      }
      btn.disabled = true;
      try {
        const made = await post(`projects/${p.id}/repos`, b);
        adding = null;
        await load();
        toast(`Added ${made.name}` + (made.folder ? '' : ' · cloning'));
      } catch (e2) {
        err.textContent = e2.message;
      } finally {
        btn.disabled = false;
      }
    };
    return h('div', { class: 'repo-add' },
      seg([['github', 'GitHub repo'], ['folder', 'Folder on ' + here]], kind, (v) => ((kind = v), drawWhere())),
      where,
      h('div', { class: 'row2' }, field('Name', name), field('Base branch', base)),
      err,
      h('div', { class: 'row-btns' }, h('button', { class: 'btn primary sm', type: 'button', onclick: submit }, 'Add repository'), h('button', { class: 'btn sm', type: 'button', onclick: () => ((adding = null), draw()) }, 'Cancel')));
  }

  function draw() {
    box.replaceChildren(...[
      kicker('Repositories', h('small', { class: 'faint' }, 'threads choose which one they work on')),
      h('div', { class: 'repos-list' }, repos.map(rowOf)),
      repos.length < 2 && !adding ? h('p', { class: 'faint small' }, `Just the one so far. Add more GitHub repositories, or folders on ${here}, and the lead can send each thread to the right one (its branch, merge and pull request go there).`) : null,
      adding || h('button', { class: 'btn sm', type: 'button', onclick: () => ((adding = addForm()), draw()) }, ic('plus', 12), 'Add a repository or folder')].filter(Boolean));
  }

  draw();
  load();
  // reload when a clone finishes or another window changes the list; stop once the settings tab is gone
  const unlisten = listen((ev) => {
    if (!box.isConnected) return ((dead = true), unlisten());
    if (ev.kind === 'repos' && ev.project === p.id && !adding) load();
  });
  return box;
}

export { projectOf, ago, modelOptions };
