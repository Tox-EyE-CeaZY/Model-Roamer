// Home: what needs you, what is running, the projects, the fleet at a glance.

import { h, ago, plural } from '../util.js';
import { icon } from '../icons.js';
import { get, post, listen } from '../api.js';
import { S, watch, agentList, agentName } from '../store.js';
import { go, openThread, projectMenu, newProject } from '../app.js';
import { dialog, toast } from '../ui.js';
import { ic, stamp, statusChip, empty, field, input, textarea, seg, agentModelPicker, pickFolder, folderInput, timeAgo, loading, kicker } from '../common.js';
import { attention as sortOut, markSeen, unmarkSeen, watchAttention } from '../attention.js';

export default async function home(el) {
  const attention = h('section', { class: 'h-attn' });
  const running = h('section', { class: 'h-run' });
  const projects = h('section', { class: 'h-projects' });
  const fleet = h('section', { class: 'h-fleet' });
  const chats = h('section', { class: 'h-chats' });

  const hour = new Date().getHours();
  const hello = hour < 5 ? 'Up late' : hour < 12 ? 'Good morning' : hour < 18 ? 'Good afternoon' : 'Good evening';
  el.append(
    h('div', { class: 'hero' },
      h('div', {},
        h('div', { class: 'eyebrow' }, S.me.local ? 'This computer' : `Connected to ${S.server.name}`),
        h('h1', {}, hello + '.'),
        h('p', { class: 'lede' }, 'Give a project to a lead agent and it plans the work, hands pieces to Claude Code, Codex and Antigravity, and keeps them out of each other’s way.')),
      h('div', { class: 'hero-acts' },
        h('button', { class: 'btn primary', onclick: () => newProject() }, ic('plus', 14), 'New project'),
        h('button', { class: 'btn', onclick: () => import('./chats.js').then((m) => m.newChat()) }, ic('chat', 14), 'New chat'))),
    attention, running,
    h('div', { class: 'h-cols' }, projects, h('div', { class: 'h-aside' }, fleet, chats)),
  );

  let threads = [];
  async function loadThreads() {
    const lists = await Promise.all(S.projects.slice(0, 20).map((p) => get(`projects/${p.id}/threads`).catch(() => [])));
    threads = lists.flat();
  }

  // What waits on the person (open questions, blocked/failed/limited threads, an unread lead reply) is loud; workers
  // that finished report to the lead, so they sit in a quiet, folded "Finished" group. attention.js decides which.
  let finOpen = false;
  try {
    finOpen = localStorage.getItem('mr-fin-open') === '1';
  } catch { /* fine */ }
  const pname = (t) => S.projects.find((p) => p.id === t.project_id)?.name || '';
  const seenFor = (list, what) => {
    markSeen(list);
    toast(what, { action: 'Undo', ms: 7000, run: () => unmarkSeen(list) });
  };
  const seenBtn = (t, label = 'Mark seen') => h('button', { class: 'ibtn xs attn-x', type: 'button', title: label, 'aria-label': label,
    onclick: (e) => (e.stopPropagation(), seenFor(t, 'Marked seen. It comes back if it changes.')), html: icon('close', 12) });
  const row = (cls, go, ...kids) => h('div', { class: cls, role: 'button', tabindex: '0', onclick: go, onkeydown: (e) => (e.key === 'Enter' || e.key === ' ') && (e.preventDefault(), go()) }, ...kids);

  function drawAttention() {
    const decisions = S.projects.filter((p) => p.decisions);
    const { need, finished, count } = sortOut(threads, decisions.reduce((n, p) => n + p.decisions, 0));
    const parts = [];
    if (count) {
      parts.push(kicker('Waiting on you', h('b', { class: 'count hot' }, count)),
        h('div', { class: 'attn-list' },
          decisions.map((p) => h('button', { class: 'attn cut', style: { '--ph': p.color }, onclick: () => go('#/p/' + p.id) }, ic('question', 18), h('div', {}, h('b', {}, plural(p.decisions, 'decision') + ' to make'), h('small', {}, p.name)), ic('right', 14))),
          need.map((t) => t.kind === 'lead'
            ? row('attn cut', () => go('#/p/' + t.project_id), ic('chat', 18), h('div', {}, h('b', {}, 'The lead replied'), h('small', {}, pname(t) + (t.last_body ? ' · ' + t.last_body.replace(/[#*_`>]/g, '').slice(0, 90) : ''))), h('span', { class: 'status hot' }, 'Unread'), seenBtn(t))
            : row('attn cut', () => openThread(t.id), ic('warn', 18), h('div', {}, h('b', {}, t.title), h('small', {}, pname(t) + (t.status_note ? ' · ' + t.status_note : ''))), statusChip(t), seenBtn(t)))));
    }
    if (finished.length) {
      const toggle = () => {
        finOpen = !finOpen;
        try {
          localStorage.setItem('mr-fin-open', finOpen ? '1' : '');
        } catch { /* fine */ }
        drawAttention();
      };
      parts.push(h('div', { class: 'fin' + (finOpen ? ' open' : '') },
        h('div', { class: 'fin-h' },
          h('button', { class: 'fin-t', type: 'button', 'aria-expanded': String(finOpen), onclick: toggle, title: 'Workers that finished and reported to their lead. The lead reviews them; nothing here needs you.' },
            h('span', {}, 'Finished'), h('b', { class: 'fin-n' }, finished.length), h('small', {}, 'the lead reviews these'), h('i', { class: 'lane-chev' + (finOpen ? ' open' : ''), html: icon('chevDown', 12) })),
          finOpen ? h('button', { class: 'chipbtn', type: 'button', onclick: () => seenFor(finished, `Marked ${plural(finished.length, 'thread')} seen`) }, 'Mark all seen') : null),
        finOpen ? h('div', { class: 'fin-list' }, finished.map((t) => row('fin-i', () => openThread(t.id),
          ic('check', 14), h('div', {}, h('span', { class: 'fin-title' }, t.title), h('small', {}, pname(t) + (t.status_note ? ' · ' + t.status_note : ''))),
          stamp(t.agent, { small: true }), seenBtn(t)))) : null));
    }
    attention.replaceChildren(...parts);
  }

  function drawRunning() {
    const live = threads.filter((t) => S.running[t.id] || ['working', 'queued'].includes(t.status));
    if (!live.length) return running.replaceChildren();
    running.replaceChildren(kicker('Working now', h('b', { class: 'count' }, live.length)),
      h('div', { class: 'run-list' }, live.map((t) => h('button', { class: 'run-i', onclick: () => openThread(t.id) },
        h('i', { class: 'pulse' }), h('span', { class: 'ri-t' }, t.title), h('small', {}, t.status_note || S.projects.find((p) => p.id === t.project_id)?.name || ''), stamp(t.agent, { small: true })))));
  }

  function drawProjects() {
    if (!S.projects.length) {
      projects.replaceChildren(kicker('Projects'), empty('board', 'No projects yet', 'A project is a folder or a GitHub repository, a lead agent and its threads. The lead keeps memory, history and context for the whole team.', h('button', { class: 'btn primary', onclick: () => newProject() }, 'Make the first project')));
      return;
    }
    projects.replaceChildren(kicker('Projects', h('button', { class: 'chipbtn', onclick: () => newProject() }, '+ New')),
      h('div', { class: 'pgrid' }, S.projects.map((p) => {
        const mine = threads.filter((t) => t.project_id === p.id && t.kind !== 'lead');
        const done = mine.filter((t) => t.status === 'done' || t.resolved).length;
        return h('a', { class: 'pcard cut', href: '#/p/' + p.id, style: { '--ph': p.color }, oncontextmenu: (e) => (e.preventDefault(), projectMenu(p, e.clientX, e.clientY)) },
          h('div', { class: 'pc-top' }, h('i', { class: 'pc-sw' }), h('h3', {}, p.name), p.decisions ? h('b', { class: 'count hot', title: 'Decisions waiting' }, p.decisions) : null),
          h('div', { class: 'pc-where mono' }, ic(p.repo_url ? 'branch' : 'folder', 13), h('span', {}, p.repo_url ? p.repo_url.replace(/^https?:\/\/(www\.)?github\.com\//, '').replace(/\.git$/, '') : p.folder || 'Kept by Model Roamer')),
          h('div', { class: 'pc-stats' },
            h('span', {}, h('b', {}, p.working || 0), ' working'),
            h('span', {}, h('b', {}, p.open_threads || 0), ' open'),
            h('span', {}, h('b', {}, done), ' done')),
          mine.length ? h('div', { class: 'pc-bar' }, mine.slice(0, 24).map((t) => h('i', { class: 'st-' + (S.running[t.id] ? 'working' : t.status), style: { '--ah': S.agents[t.agent]?.info?.hue ?? 200 }, title: `${t.title} · ${agentName(t.agent)}` }))) : null,
          h('div', { class: 'pc-foot' }, stamp(p.lead_agent, { model: p.lead_model, small: true, lead: true }), h('small', { class: 'faint' }, 'updated ', timeAgo(p.updated_at))));
      })));
  }

  function drawFleet() {
    const list = agentList();
    fleet.replaceChildren(kicker('The fleet', h('a', { class: 'chipbtn', href: '#/agents' }, 'Manage')),
      h('div', { class: 'fleet' }, list.map((a) => {
        const busy = threads.filter((t) => t.agent === a.id && S.running[t.id]).length;
        const state = a.pending ? ['Checking…', 'dim'] : !a.installed ? ['Not installed', 'faint'] : a.auth?.signed_in === 'no' ? ['Not signed in', 'hot'] : a.limit?.active ? ['At its limit' + (a.limit.until ? ' · back ' + new Date(a.limit.until).toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' }) : ''), 'hot'] : a.config?.enabled === false ? ['Turned off', 'faint'] : [busy ? `${busy} running` : 'Ready', busy ? 'live' : 'acc'];
        return h('a', { class: 'fl-i', href: '#/agents?open=' + a.id, style: { '--ah': a.info?.hue } }, h('i', { class: 'fl-sw' }), h('b', {}, a.info?.name), h('span', { class: 'status ' + state[1] }, state[0]));
      })));
  }

  async function drawChats() {
    let list = [];
    try {
      list = (await get('threads?kind=chat')).slice(0, 6);
    } catch { /* fine */ }
    chats.replaceChildren(kicker('Recent chats', h('a', { class: 'chipbtn', href: '#/chats' }, 'All')),
      list.length
        ? h('div', { class: 'mini-list' }, list.map((t) => h('button', { class: 'mini', onclick: () => openThread(t.id) }, h('span', {}, t.title), stamp(t.agent, { small: true }), h('small', { class: 'faint' }, ago(t.updated_at)))))
        : h('p', { class: 'faint small' }, 'One-to-one chats with any agent, outside a project. Ctrl+N starts one.'));
  }

  async function refresh() {
    await loadThreads();
    drawAttention();
    drawRunning();
    drawProjects();
    drawFleet();
  }
  projects.replaceChildren(loading());
  await refresh();
  drawChats();
  const unwatch = watch((what) => (what === 'agents' ? drawFleet() : what === 'running' ? (drawRunning(), drawFleet()) : refresh()));
  const unlisten = listen((ev) => ev.kind === 'thread' && refresh());
  const unseen = watchAttention(drawAttention);
  return () => (unwatch(), unlisten(), unseen());
}

// ------------------------------------------------------------ new project
export async function newProjectDialog(preset = {}) {
  let source = preset.repo ? 'github' : 'folder';
  let folder = preset.folder || '', repo = preset.repo || '';
  const name = input(preset.name || '', { placeholder: 'What is this project called?' });
  const folderIn = input(folder, { placeholder: 'C:\\Users\\you\\code\\thing  or  /home/you/thing' });
  const browse = h('button', { class: 'btn', type: 'button', onclick: async () => {
    const p = await pickFolder({ start: folderIn.value });
    if (p) {
      folderIn.value = p;
      if (!name.value.trim()) name.value = p.split(/[\\/]/).filter(Boolean).pop() || '';
    }
  } }, ic('folder', 14), 'Browse');
  const repoIn = input(repo, { placeholder: 'https://github.com/you/repo' });
  const repoList = h('div', { class: 'repo-list' });
  const lead = agentModelPicker({ agentId: S.settings.default_lead || 'claude', model: S.settings.default_lead_model ?? 'opus', leadOnly: true });
  const instr = textarea(preset.instructions || '', { rows: 4, placeholder: 'Standing rules for every agent in this project: goals, style, things to never do, how to test. Optional.' });
  const create = toggleCreate();
  function toggleCreate() {
    const b = h('label', { class: 'chk' }, h('input', { type: 'checkbox' }), h('span', {}, 'Make the folder if it doesn’t exist'));
    return b;
  }
  const where = h('div', { class: 'np-where' });
  // more repositories beside the first: GitHub repositories and folders, added in one go
  const extras = [];
  const moreBox = h('div', { class: 'np-more' });
  const here = S.server?.headless || !S.me?.local ? 'the server' : 'this computer';
  function addExtra(kind, value = '') {
    const x = { kind };
    if (kind === 'github') x.inp = input(value, { placeholder: 'https://github.com/you/another-repo  or  you/another-repo' });
    else x.ctl = folderInput({ value });
    extras.push(x);
    drawMore();
    (x.inp || x.ctl.input).focus();
    return x;
  }
  function drawMore() {
    moreBox.replaceChildren(
      h('span', { class: 'f-l' }, 'More repositories', h('small', { class: 'faint' }, ' optional · threads choose which one they work on')),
      ...extras.map((x) => h('div', { class: 'np-extra' },
        h('span', { class: 'np-x-ic', title: x.kind === 'github' ? 'GitHub repository' : 'Folder on ' + here }, ic(x.kind === 'github' ? 'branch' : 'folder', 14)),
        h('div', { class: 'grow' }, x.inp || x.ctl.el),
        h('button', { class: 'ibtn sm', type: 'button', title: 'Remove', onclick: () => (extras.splice(extras.indexOf(x), 1), drawMore(), markRepos()), html: icon('close', 14) }))),
      h('div', { class: 'row-btns' },
        h('button', { class: 'btn sm', type: 'button', onclick: () => addExtra('github') }, ic('plus', 12), 'GitHub repo'),
        h('button', { class: 'btn sm', type: 'button', onclick: () => addExtra('folder') }, ic('plus', 12), 'Folder on ' + here)));
  }
  const chosen = () => new Set([repoIn.value.trim(), ...extras.filter((x) => x.inp).map((x) => x.inp.value.trim())].filter(Boolean));
  const markRepos = () => repoList.querySelectorAll('.repo-i').forEach((b) => b.classList.toggle('on', chosen().has(b.dataset.url)));
  drawMore();
  const drawWhere = () => {
    if (source === 'folder') where.replaceChildren(field('Folder', h('div', { class: 'row' }, folderIn, browse), 'Agents work in this folder. Git repositories get a worktree per worker so they never collide.'), create);
    else if (source === 'github') {
      where.replaceChildren(field('Repository', repoIn, 'Model Roamer clones it into its own data folder. Sign in to GitHub in Settings for private repositories.'), repoList);
      loadRepos();
    } else where.replaceChildren(h('p', { class: 'faint small' }, 'Model Roamer keeps the files in its own data folder. You can point the project at a folder later.'));
  };
  let reposLoaded = false;
  async function loadRepos() {
    if (reposLoaded) return;
    reposLoaded = true;
    repoList.replaceChildren(loading('Looking at your GitHub repositories'));
    try {
      const list = await get('github/repos');
      // pick several: the first is the project's own, the rest are added beside it (click again to take one off)
      repoList.replaceChildren(h('small', { class: 'faint' }, 'Pick one or several. The first is the primary; Model Roamer clones them all.'), ...list.slice(0, 40).map((r) => h('button', { class: 'repo-i', type: 'button', 'data-url': r.url, onclick: () => {
        const ex = extras.find((x) => x.inp?.value.trim() === r.url);
        if (repoIn.value.trim() === r.url) {
          // the primary taken off: the next one picked becomes the primary
          const next = extras.find((x) => x.inp?.value.trim());
          repoIn.value = next ? next.inp.value.trim() : '';
          if (next) extras.splice(extras.indexOf(next), 1);
          drawMore();
        } else if (ex) {
          extras.splice(extras.indexOf(ex), 1);
          drawMore();
        } else if (!repoIn.value.trim()) {
          repoIn.value = r.url;
          if (!name.value.trim()) name.value = r.full_name.split('/')[1];
        } else addExtra('github', r.url);
        markRepos();
      } }, ic(r.private ? 'lock' : 'branch', 13), h('span', {}, r.full_name), h('small', { class: 'faint' }, r.pushed ? ago(Date.parse(r.pushed)) : ''))));
      markRepos();
    } catch {
      repoList.replaceChildren(h('p', { class: 'faint small' }, 'Paste a repository address above. (Sign in to GitHub under Settings › GitHub to pick from a list.)'));
    }
  }
  const body = h('div', { class: 'dlg-fields np' },
    field('Name', name),
    h('div', { class: 'field' }, h('span', { class: 'f-l' }, 'Where the code lives'), seg([['folder', 'A folder'], ['github', 'A GitHub repo'], ['none', 'Start empty']], source, (v) => ((source = v), drawWhere()))),
    where,
    moreBox,
    field('Lead', lead.el, 'The lead plans, delegates, reviews and keeps the project’s memory. Claude, Codex and Antigravity can all lead.'),
    field('Project instructions', instr));
  drawWhere();
  let made = null;
  const ok = await dialog({
    title: 'New project',
    wide: true,
    body,
    ok: 'Create project',
    check: async () => {
      if (!name.value.trim()) return 'Give the project a name.';
      const { agent: la, model: lm } = lead.get();
      const b = { name: name.value.trim(), instructions: instr.value, lead_agent: la, lead_model: lm };
      if (source === 'folder') {
        if (!folderIn.value.trim()) return 'Choose a folder (or pick “Start empty”).';
        b.folder = folderIn.value.trim();
        b.create_folder = create.querySelector('input').checked;
      } else if (source === 'github') {
        if (!repoIn.value.trim()) return 'Paste the repository address, or pick one.';
        b.repo_url = repoIn.value.trim();
      }
      b.repos = extras.map((x) => (x.inp ? { url: x.inp.value.trim() } : { folder: x.ctl.get() })).filter((r) => r.url || r.folder);
      try {
        made = await post('projects', b);
      } catch (e) {
        return e.message;
      }
    },
  });
  if (ok && made) {
    toast(`Made ${made.name}`);
    go('#/p/' + made.id);
  }
  return made;
}

export { icon };
