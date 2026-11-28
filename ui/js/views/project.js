// A project: the chat with its lead on the left, its threads in lanes on the right, plus files, git, memory,
// artifacts and settings tabs. Handles both owner projects and joined collaborative projects with the exact same
// layout and components, hiding any tab, button, menu item or shortcut the collaborator isn't permitted to use.

import { h, ago } from '../util.js';
import { icon } from '../icons.js';
import { get, post, patch, del, listen } from '../api.js';
import { S, watch, agentName, agentHue, project as projectOf, isJoinedProject, collabLink, addRef } from '../store.js';
import { go, back, openThread, deleteProject, leaveJoinedProject, parseRoute } from '../app.js';
import { popOut, setTitle, openExternal } from '../native.js';
import { dialog, toast, showMenu, overlaid } from '../ui.js';
import { md } from '../md.js';
import { mountThread, decisionCard } from './thread.js';
import { ic, stamp, statusChip, field, input, textarea, select, seg, toggle, agentModelPicker, routeEffort, agentOptions, modelOptions, pickFolder, folderInput, askLine, repoSelect, repoLabel, loading, errorBox, empty, timeAgo, kicker, copyable } from '../common.js';
import { needsYou, isFinished } from '../attention.js';
import { scrollStrip } from '../scrollstrip.js';
import { mountPresence, mountProjectInbox, ownerOverlap, drawPresence, drawInbox, clearControl, presenceDot, requestedInbox } from '../collab-presence.js';
import { mountBoardChat, ensureLocal, threadOwnerLabel } from './collab_chat.js';
import { projectAccess, accessFromStore, rememberProjectTab, savedProjectTab, collabPrefs, setCollabPref, ROLE_LABEL } from '../collab_perms.js';

const enc = encodeURIComponent;
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

// Track the last project id we rendered so sidebar/Home navigation restores the saved tab,
// while clicking the Board tab from within the same project explicitly stays on Board.
let activeProjectId = '';

export default async function projectView(el, r, ctx) {
  if (isJoinedProject(r.id)) {
    return joinedProjectView(el, r, ctx);
  }
  return ownerProjectView(el, r, ctx);
}

// ================================================================================================ owner project
async function ownerProjectView(el, r, ctx) {
  let p;
  try {
    p = await get('projects/' + r.id);
  } catch (e) {
    if (e.status === 404 || /no such/i.test(e.message)) return el.replaceChildren(empty('board', 'That project isn’t here', 'It may have been deleted.', h('a', { class: 'btn', href: '#/' }, 'Home')));
    throw e;
  }
  const access = projectAccess({ isJoined: false });
  const rawTab = r.tab === 'board' ? '' : (r.tab || '');
  const entering = activeProjectId !== p.id;
  activeProjectId = p.id;
  if (entering && !rawTab && !r.q?.t && !r.q?.inbox) {
    const saved = savedProjectTab(p.id, access);
    if (saved) {
      history.replaceState(history.state, '', `#/p/${p.id}/${saved}`);
      return ownerProjectView(el, { ...r, tab: saved }, ctx);
    }
  }
  const tab = access.canTab(rawTab) ? rawTab : '';
  rememberProjectTab(p.id, tab);

  setTitle(`${p.name} · Model Roamer`);
  const headEl = h('div', { class: 'p-head', style: { '--ph': p.color } });
  const presenceHost = h('div', { class: 'collab-presence-host' });
  const inboxHost = h('div', { class: 'collab-note-host' });
  const collabStrip = h('div', { class: 'collab-toolbar' }, presenceHost, inboxHost);
  const stopPresence = mountPresence(presenceHost, p.id);
  const inbox = mountProjectInbox(inboxHost, p.id);
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
    const needs = threadBadgeState ? threadBadgeState.needsYou : (p.decisions?.length ?? sp?.decisions ?? 0);
    const unread = threadBadgeState ? threadBadgeState.unread : (sp?.unread ?? 0);
    const extraUnread = threadBadgeState ? threadBadgeState.extraUnread : 0;
    const nodes = [];
    if (needs > 0) {
      nodes.push(h('b', {
        class: 'p-tab-badge hot',
        title: `${needs} need you` + (unread ? ` · ${unread} unread` : ''),
      }, needs));
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
    rememberProjectTab(p.id, '');
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
      ...access.tabs.map(([k, label, ico]) => h('a', {
        class: 'p-tab' + (k === '' ? ' p-tab-desk' : '') + (k === tab ? ' on' : ''),
        href: '#/p/' + p.id + (k ? '/' + k : ''),
        onclick: () => rememberProjectTab(p.id, k),
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
              { label: 'People & collaborators…', icon: 'people', run: () => go(`#/p/${p.id}/settings?section=people`) },
              { label: 'Pop the project chat out', icon: 'popout', run: () => popOut('#/t/' + p.lead.id, { key: 't:' + p.lead.id }) },
              p.home && S.me.local ? { label: 'Show the folder', icon: 'folder', run: () => post('fs/reveal', { path: p.home }) } : null,
              p.repo_url ? { label: 'Open on GitHub', icon: 'external', run: () => openExternal(p.repo_url.replace(/\.git$/, '')) } : null,
              { sep: true },
              { label: 'Delete project', icon: 'trash', danger: true, run: () => deleteProject(p) },
            ].filter(Boolean), b.right, b.bottom + 4, { anchor: e.currentTarget });
          }, html: icon('more', 16) }))),
      collabStrip, tabStrip.el);
    renderThreadBadge();
  }
  drawHead();

  const onKey = projectShortcuts(() => access, p.id);
  document.addEventListener('keydown', onKey);

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
    inner = await board(body, p, r, { ...ctx, onRoute: (update) => ctx?.onRoute?.((next) => {
      if (next.q.inbox) inbox.open();
      return update(next);
    }) }, {
      getPane: () => mobilePane,
      onPane: (fn) => (onPaneChange = fn),
      onBadge: (info) => {
        threadBadgeState = info;
        renderThreadBadge();
      },
    });
  } else if (tab === 'files') inner = await (await import('./files.js')).default(body, p, r);
  else if (tab === 'git') inner = await gitTab(body, p, r.q?.repo || '');
  else if (tab === 'memory') inner = await (await import('./memory.js')).default(body, { q: {} }, { project: p.id });
  else if (tab === 'artifacts') inner = await (await import('./artifacts.js')).default(body, { q: {} }, { project: p.id });
  else if (tab === 'settings') inner = await settingsTab(body, p, (np) => ((p = { ...p, ...np }), drawHead()), r);
  return () => {
    const next = parseRoute();
    if (next.view !== 'project' || next.id !== p.id) activeProjectId = '';
    document.removeEventListener('keydown', onKey);
    unlisten();
    unwatch();
    inbox.destroy();
    stopPresence();
    tabStrip?.destroy();
    inner?.();
  };
}

function projectShortcuts(getAccess, projectId) {
  return (e) => {
    if (overlaid() || document.querySelector('.pal')) return;
    if (e.target.matches?.('input, textarea, select, [contenteditable], .xterm-helper-textarea')) return;
    const acc = getAccess();
    if (acc.revoked) return;
    if (e.altKey && !e.ctrlKey && !e.metaKey && !e.shiftKey && e.key >= '1' && e.key <= '6') {
      const idx = Number(e.key) - 1;
      const target = acc.tabs[idx];
      if (target) {
        e.preventDefault();
        rememberProjectTab(projectId, target[0]);
        go(`#/p/${enc(projectId)}${target[0] ? '/' + target[0] : ''}`);
      }
      return;
    }
    if (!e.altKey && !e.ctrlKey && !e.metaKey && !e.shiftKey && e.key === 'n' && acc.canCreateThread) {
      e.preventDefault();
      newThreadDialog(projectId);
    }
  };
}

// ================================================================================================ joined project
async function joinedProjectView(el, r, ctx) {
  const linkId = r.id;
  const base = `collab/links/${enc(linkId)}/host/`;
  const viewBase = `collab/links/${enc(linkId)}/view/`;

  let link = collabLink(linkId);
  if (!link) {
    const links = await get('collab/links').catch(() => []);
    link = links.find((l) => l.id === linkId) || null;
  }
  if (!link) {
    return el.replaceChildren(empty('people', 'Not joined', 'This project isn’t in your list any more.', h('a', { class: 'btn', href: '#/' }, 'Home')));
  }

  const headEl = h('div', { class: 'p-head', style: { '--ph': 128 } });
  const presenceHost = h('div', { class: 'collab-presence-host' });
  const inboxHost = h('div', { class: 'collab-note-host' });
  const collabStrip = h('div', { class: 'collab-toolbar' }, presenceHost, inboxHost);
  const body = h('div', { class: 'p-body' });
  el.append(headEl, body);
  body.replaceChildren(loading('Opening ' + link.project_name));

  let alive = true;
  let live = link.status === 'revoked' ? 'revoked' : 'ok';
  let ensured = null;
  let hostState = null;
  let localProject = null;
  let hasLocalSource = false;
  let hasGitSource = false;
  let perms = link.perms && typeof link.perms === 'object' ? { ...link.perms } : {};
  let catalog = [];
  let me = { id: link.member_id, user_id: link.user_id, name: link.name, role: link.role, requested_role: '' };
  let people = [];
  let presence = null;
  let notes = [];
  let cursor = 0;
  let openInbox = requestedInbox();
  const hostThreads = new Map();
  const hostMsgs = new Map();
  const liveListeners = new Set();
  const onLiveChange = (fn) => (liveListeners.add(fn), () => liveListeners.delete(fn));
  const notifyLive = (ev) => liveListeners.forEach((fn) => { try { fn(ev); } catch (e) { console.error(e); } });

  const isRevokedErr = (e) => e?.status === 403 && /removed|withdrawn|isn’t valid|isn't valid|left it/i.test(e?.message || '');
  const samePerms = (a, b) => {
    const ka = Object.keys(a || {}).sort();
    const kb = Object.keys(b || {}).sort();
    return ka.length === kb.length && ka.every((k, i) => k === kb[i] && !!a[k] === !!b[k]);
  };
  let lastPermToastAt = 0;

  // Bootstrap: ensure local side + fetch host project state (resilient offline/revoked)
  try {
    ensured = await post(viewBase + 'ensure', {}, { timeout: 25000 });
    if (ensured?.link) link = ensured.link;
    hasLocalSource = !!ensured?.has_local_source;
    hasGitSource = !!ensured?.has_git_source;
    if (ensured?.revoked) live = 'revoked';
    else if (ensured?.offline) live = 'offline';
  } catch (e) {
    if (isRevokedErr(e)) live = 'revoked';
    else live = 'offline';
  }

  localProject = await get('projects/' + enc(linkId), { quiet: true }).catch(() => null);

  if (live !== 'revoked') {
    try {
      hostState = await get(base + 'project', { timeout: 25000 });
      live = 'ok';
      perms = { ...(hostState.perms || {}) };
      catalog = hostState.catalog || [];
      me = { ...me, ...(hostState.member || {}) };
      people = hostState.people || [];
      presence = hostState.presence || null;
      notes = hostState.notes || [];
      cursor = hostState.cursor || 0;
      (hostState.threads || []).forEach((t) => hostThreads.set(t.id, t));
    } catch (e) {
      if (isRevokedErr(e)) live = 'revoked';
      else live = 'offline';
    }
  }

  if (!alive) return;

  const projectName = () => hostState?.project?.name || localProject?.name || link.project_name || 'Shared project';
  const hostName = () => hostState?.host?.name || link.host_name || 'Host';
  const projectColor = () => hostState?.project?.color ?? localProject?.color ?? 128;
  const repoUrl = () => hostState?.project?.repo_url || localProject?.repo_url || '';
  const baseBranch = () => hostState?.project?.base_branch || localProject?.base_branch || '';
  const hasOwnThreads = () => {
    for (const t of hostThreads.values()) {
      if (!t.deleted_at && t.user_id === me.user_id && t.kind !== 'chat' && t.kind !== 'lead') return true;
    }
    return false;
  };
  const getAccess = () => projectAccess({
    isJoined: true,
    perms,
    role: me.role || link.role,
    status: live,
    hasLocalSource,
    hasGitSource,
    hasOwnThreads: hasOwnThreads(),
    userId: me.user_id || link.user_id || '',
  });

  el.style.setProperty('--ph', projectColor());
  headEl.style.setProperty('--ph', projectColor());
  setTitle(`${projectName()} · Model Roamer`);

  // Tab selection & last-tab memory
  let access = getAccess();
  const rawTab = r.tab === 'board' ? '' : (r.tab || '');
  const entering = activeProjectId !== linkId;
  activeProjectId = linkId;
  if (!access.revoked && entering && !rawTab && !r.q?.t && !r.q?.inbox) {
    const saved = savedProjectTab(linkId, access);
    if (saved) {
      history.replaceState(history.state, '', `#/p/${enc(linkId)}/${saved}`);
      r = { ...r, tab: saved };
    }
  }
  let tab = r.tab === 'board' ? '' : (r.tab || '');
  if (!access.revoked && access.canTab(tab)) rememberProjectTab(linkId, tab);

  let mobilePane = savedPane(linkId);
  let onPaneChange = null;
  let threadBadgeState = null;
  let tabStrip = null;
  let leadTabEl = null;
  let threadsTabEl = null;
  let threadsBadgeHost = null;
  let innerStop = null;
  let boardChatHandle = null;

  async function refreshNotes() {
    try {
      notes = await get(base + 'notes', { quiet: true });
      if (alive) drawCoordination();
    } catch (e) {
      toast(e.message, { err: true });
    }
  }

  function drawCoordination() {
    const acc = getAccess();
    const prefs = collabPrefs(linkId);
    if (acc.canViewPresence && prefs.show_presence) {
      drawPresence(presenceHost, presence, live === 'ok', linkId);
    } else {
      clearControl(presenceHost);
    }
    if (!acc.revoked) {
      drawInbox(inboxHost, base, live === 'ok' ? notes : [], refreshNotes, openInbox);
      if (live === 'ok' && notes.length) openInbox = false;
    } else {
      clearControl(inboxHost);
    }
  }

  function renderThreadBadge() {
    if (!threadsBadgeHost) return;
    const sp = projectOf(linkId);
    const needs = threadBadgeState ? threadBadgeState.needsYou : (sp?.decisions ?? 0);
    const unread = threadBadgeState ? threadBadgeState.unread : (sp?.unread ?? 0);
    const extraUnread = threadBadgeState ? threadBadgeState.extraUnread : 0;
    const nodes = [];
    if (needs > 0) {
      nodes.push(h('b', { class: 'p-tab-badge hot', title: `${needs} need you` + (unread ? ` · ${unread} unread` : '') }, needs));
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
    rememberPane(linkId, next);
    rememberProjectTab(linkId, '');
    if (tab === '' && onPaneChange) {
      e?.preventDefault();
      syncMobileTabHighlight();
      onPaneChange(next);
      tabStrip?.revealActive({ smooth: true });
    }
  }

  function drawHead() {
    access = getAccess();
    tabStrip?.destroy();
    const liveChip = live === 'ok'
      ? h('span', { class: 'status live' }, h('i', { class: 'pulse' }), 'Live')
      : live === 'revoked'
        ? h('span', { class: 'status hot' }, 'Removed')
        : h('span', { class: 'status dim' }, 'Offline, retrying');

    threadsBadgeHost = h('span', { class: 'p-tab-badges' });
    leadTabEl = !access.revoked ? h('a', {
      class: 'p-tab p-tab-mob' + (tab === '' && mobilePane === 'lead' ? ' on' : ''),
      href: '#/p/' + enc(linkId),
      onclick: (e) => pickMobilePane('lead', e),
    }, ic('chat', 14), 'Your chat') : null;
    threadsTabEl = !access.revoked && access.canSeeThreads ? h('a', {
      class: 'p-tab p-tab-mob' + (tab === '' && mobilePane === 'threads' ? ' on' : ''),
      href: '#/p/' + enc(linkId),
      onclick: (e) => pickMobilePane('threads', e),
    }, ic('board', 14), 'Threads', threadsBadgeHost) : null;

    const nav = access.tabs.length ? h('nav', { class: 'p-tabs', 'aria-label': 'Project sections' },
      leadTabEl,
      threadsTabEl,
      ...access.tabs.map(([k, label, ico]) => h('a', {
        class: 'p-tab' + (k === '' ? ' p-tab-desk' : '') + (k === tab ? ' on' : ''),
        href: '#/p/' + enc(linkId) + (k ? '/' + k : ''),
        onclick: () => rememberProjectTab(linkId, k),
      }, ic(ico, 14), label))) : null;

    tabStrip = nav ? scrollStrip(nav) : null;

    const moreItems = () => [
      access.canCreateThread ? { label: 'New thread…', icon: 'plus', run: () => newThreadDialog(linkId) } : null,
      !access.revoked && (ensured?.thread?.id || localProject?.lead?.id) ? { label: 'Pop your project chat out', icon: 'popout', run: () => popOut('#/t/' + (ensured?.thread?.id || localProject.lead.id), { key: 't:' + (ensured?.thread?.id || localProject.lead.id) }) } : null,
      !access.revoked && repoUrl() ? { label: 'Open on GitHub', icon: 'external', run: () => openExternal(repoUrl().replace(/\.git$/, '')) } : null,
      { sep: true },
      { label: 'Leave project', icon: 'logout', danger: true, run: () => leaveJoinedProject({ ...link, project_name: projectName(), host_name: hostName() }) },
    ].filter(Boolean);

    headEl.replaceChildren(
      h('div', { class: 'p-title' },
        h('i', { class: 'p-sw' }),
        h('h1', {}, projectName()),
        h('span', { class: 'p-sub mono' },
          ic('people', 13),
          h('span', {}, `Hosted by ${hostName()}`),
          h('span', { class: 'tag' }, ROLE_LABEL[access.role] || access.role),
          liveChip),
        h('span', { class: 'grow' }),
        h('div', { class: 'p-acts' },
          access.canCreateThread
            ? h('button', { class: 'btn primary sm', title: 'New thread (N)', 'aria-label': 'New thread', onclick: () => newThreadDialog(linkId) }, ic('plus', 13), h('span', { class: 'p-new-lbl' }, 'New thread'))
            : null,
          h('button', {
            class: 'ibtn',
            title: 'More',
            'aria-label': 'More project actions',
            onclick: (e) => {
              const b = e.currentTarget.getBoundingClientRect();
              showMenu(moreItems(), b.right, b.bottom + 4, { anchor: e.currentTarget });
            },
            html: icon('more', 16),
          }))),
      collabStrip,
      tabStrip ? tabStrip.el : null,
    );
    drawCoordination();
    renderThreadBadge();
  }

  function drawRevokedState() {
    innerStop?.();
    innerStop = null;
    drawHead();
    body.replaceChildren(
      h('div', { class: 'collab-state-wrap' },
        h('section', { class: 'panel cut collab-removed' },
          empty(
            'lock',
            `You were removed from ${projectName()}`,
            `The owner (${hostName()}) removed your access to this project, or your membership ended. Ask them for a new invite link if you need access again.`,
            h('div', { class: 'row' },
              h('button', { class: 'btn danger', onclick: () => leaveJoinedProject({ ...link, project_name: projectName(), host_name: hostName() }) }, ic('logout', 14), 'Remove from my list'),
              h('a', { class: 'btn', href: '#/' }, 'Back to Home')),
          ))),
    );
  }

  function drawForbiddenTab(tabName) {
    innerStop?.();
    innerStop = null;
    body.replaceChildren(
      h('div', { class: 'collab-state-wrap' },
        h('section', { class: 'panel cut' },
          empty(
            'shield',
            `${tabName} isn’t available`,
            'You don’t have permission to view this section in this shared project, or there is no local repository available for it.',
            h('a', { class: 'btn primary', href: '#/p/' + enc(linkId) }, 'Go to Board'),
          ))),
    );
  }

  const collabCtx = {
    linkId,
    link,
    base,
    viewBase,
    getAccess,
    onLiveChange,
    getMe: () => me,
    getPeople: () => people,
    getPresence: () => presence,
    getCatalog: () => catalog,
    getLive: () => live,
    getHostName: hostName,
    getProjectName: projectName,
    redrawHead: drawHead,
    redrawCoordination: drawCoordination,
  };

  async function mountBody() {
    innerStop?.();
    innerStop = null;
    boardChatHandle = null;
    access = getAccess();
    if (access.revoked) return drawRevokedState();

    if (!access.canTab(tab)) {
      const label = tab ? tab[0].toUpperCase() + tab.slice(1) : 'Section';
      return drawForbiddenTab(label);
    }

    body.replaceChildren();
    if (tab === '') {
      innerStop = await joinedBoard(body, {
        linkId,
        link,
        base,
        ensured,
        localProject,
        hostThreads,
        hostMsgs,
        getAccess,
        getMe: () => me,
        getPeople: () => people,
        getLive: () => live,
        onLiveChange,
        onChatReady: (hnd) => (boardChatHandle = hnd),
      }, r, {
        ...ctx,
        onRoute: (update) => ctx?.onRoute?.((next) => {
          if (next.q?.inbox) {
            openInbox = true;
            refreshNotes();
          }
          return update(next);
        }),
      }, {
        getPane: () => (access.canSeeThreads ? mobilePane : 'lead'),
        onPane: (fn) => (onPaneChange = fn),
        onBadge: (info) => {
          threadBadgeState = info;
          renderThreadBadge();
        },
      });
    } else if (tab === 'files') {
      const projObj = localProject || { id: linkId, name: projectName(), repos: [] };
      innerStop = await (await import('./files.js')).default(body, projObj, r, { collab: collabCtx });
    } else if (tab === 'git') {
      const projObj = localProject || { id: linkId, name: projectName(), repo_url: repoUrl(), base_branch: baseBranch(), repos: [] };
      innerStop = await gitTab(body, projObj, r.q?.repo || '', { collab: collabCtx });
    } else if (tab === 'memory') {
      innerStop = await (await import('./memory.js')).default(body, { q: {} }, { project: linkId, collab: collabCtx });
    } else if (tab === 'artifacts') {
      innerStop = await (await import('./artifacts.js')).default(body, { q: {} }, { project: linkId, collab: collabCtx });
    } else if (tab === 'settings') {
      innerStop = await collabSettingsTab(body, collabCtx);
    }
  }

  drawHead();
  await mountBody();

  const onKey = projectShortcuts(getAccess, linkId);
  document.addEventListener('keydown', onKey);

  // Live host changes long-poll: updates permissions, role, threads, messages, memory, artifacts, presence, notes, and removal.
  async function pump() {
    let delay = 2000;
    while (alive && live !== 'revoked') {
      try {
        const c = await get(base + `changes?since=${cursor}&wait=20`, { timeout: 45000, quiet: true });
        if (!alive) return;
        cursor = c.cursor ?? cursor;
        const prevPerms = { ...perms };
        const prevRole = me.role;
        const prevViewThreads = !!perms.view_threads;
        const prevViewChats = !!perms.view_others_chats;

        if (c.people) {
          people = c.people;
          const selfRow = people.find((x) => x.user_id === me.user_id);
          if (selfRow?.role) me.role = selfRow.role;
        }
        if (c.presence) presence = c.presence;
        if (c.notes) notes = c.notes;
        if (c.perms && typeof c.perms === 'object') perms = { ...c.perms };

        const permsChanged = !samePerms(perms, prevPerms) || me.role !== prevRole;
        const wasOffline = live !== 'ok';
        live = 'ok';
        delay = 2000;

        if (Array.isArray(c.threads)) {
          c.threads.forEach((t) => hostThreads.set(t.id, { ...hostThreads.get(t.id), ...t }));
        }
        if (Array.isArray(c.messages)) {
          for (const m of c.messages) {
            if (!hostMsgs.has(m.thread_id)) hostMsgs.set(m.thread_id, new Map());
            hostMsgs.get(m.thread_id).set(m.id, m);
          }
        }

        if (permsChanged) {
          // Sync guest server's local link cache so server-side 403 checks, sidebar, and command palette match immediately.
          const ref = await post(viewBase + 'refresh', {}, { quiet: true, timeout: 12000 }).catch(() => null);
          if (ref) {
            if (ref.member) me = { ...me, ...ref.member };
            if (ref.perms) perms = { ...ref.perms };
            if (ref.catalog) catalog = ref.catalog;
            hasLocalSource = !!ref.has_local_source;
            hasGitSource = !!ref.has_git_source;
          }
          // If thread visibility toggled on or off, resync the full visible thread snapshot so threads appear/disappear immediately.
          if (prevViewThreads !== !!perms.view_threads || prevViewChats !== !!perms.view_others_chats) {
            const snap = await get(base + 'project', { quiet: true, timeout: 15000 }).catch(() => null);
            if (snap && alive) {
              hostThreads.clear();
              (snap.threads || []).forEach((t) => hostThreads.set(t.id, t));
              if (snap.people) people = snap.people;
              if (snap.presence) presence = snap.presence;
            }
          }
          access = getAccess();
          drawHead();
          if (collabPrefs(linkId).notify_updates && Date.now() - lastPermToastAt > 4000) {
            lastPermToastAt = Date.now();
            toast('Your project permissions were updated', { ms: 2800 });
          }
          // If current tab just became forbidden, move back to Board cleanly.
          if (tab && !access.canTab(tab)) {
            tab = '';
            rememberProjectTab(linkId, '');
            history.replaceState(history.state, '', '#/p/' + enc(linkId));
            await mountBody();
            continue;
          }
        } else if (wasOffline) {
          drawHead();
          boardChatHandle?.setPrivacy({ offline: '' });
        }

        drawCoordination();
        notifyLive({
          permsChanged,
          threadsChanged: permsChanged || (c.threads?.length > 0) || (c.messages?.length > 0),
          messages: c.messages || [],
          memoryChanged: c.memory?.length > 0,
          artifactsChanged: c.artifacts?.length > 0,
          peopleChanged: !!c.people,
        });
        if (c.more) continue;
      } catch (e) {
        if (!alive) return;
        if (isRevokedErr(e)) {
          live = 'revoked';
          drawRevokedState();
          return;
        }
        if (live !== 'offline') {
          live = 'offline';
          drawHead();
          boardChatHandle?.setPrivacy({ offline: e.message || 'Host offline' });
          notifyLive({ offline: true });
        }
        await new Promise((res) => setTimeout(res, delay));
        delay = Math.min(30000, delay * 2);
      }
    }
  }
  if (live !== 'revoked') pump();

  return () => {
    alive = false;
    const next = parseRoute();
    if (next.view !== 'project' || next.id !== linkId) activeProjectId = '';
    document.removeEventListener('keydown', onKey);
    innerStop?.();
    tabStrip?.destroy();
    clearControl(presenceHost);
    clearControl(inboxHost);
  };
}

// ================================================================================================ joined board
async function joinedBoard(el, state, r, ctx, mobile = {}) {
  const { linkId, link, base, ensured, localProject, hostThreads, hostMsgs, getAccess, getMe, getPeople, onLiveChange, onChatReady } = state;
  const leadBox = h('div', { class: 'b-lead' });
  const lanes = h('div', { class: 'b-lanes' });
  const decisionsEl = h('div', { class: 'b-decisions' });
  const claimsEl = h('div', { class: 'b-claims' });
  const boardEl = h('div', { class: 'b-right' }, decisionsEl, lanes, claimsEl);
  const peekEl = h('div', { class: 'b-peek', hidden: true });
  const split = h('div', { class: 'board', dataset: { pane: mobile.getPane?.() || 'lead' } }, leadBox, boardEl, peekEl);
  el.append(split);

  let localThreads = [];
  let localDecisions = [];
  let localClaims = [];
  let doneOpen = false;
  let finOpen = false;
  try {
    doneOpen = localStorage.getItem('mr-done-' + linkId) === '1';
    finOpen = localStorage.getItem('mr-fin-' + linkId) === '1';
  } catch { /* fine */ }

  const chatHandle = await mountBoardChat(leadBox, link, {
    ensured,
    leadAgent: localProject?.lead_agent || '',
    leadModel: localProject?.lead_model || '',
  });
  onChatReady?.(chatHandle);

  const personName = (uid) => {
    const p = getPeople().find((x) => x.user_id === uid);
    return p?.name || (uid === '' ? link.host_name : 'someone');
  };

  function mergedThreads() {
    const acc = getAccess();
    const me = getMe();
    const byId = new Map();
    // Host threads (filtered by current permissions)
    for (const t of hostThreads.values()) {
      if (t.deleted_at) continue;
      if (t.kind === 'chat' && t.user_id === me.user_id) continue; // own chat is in left pane
      const isOwn = t.user_id && t.user_id === me.user_id;
      if (!isOwn) {
        if (t.user_id && t.kind === 'chat' && !acc.canViewOthersChats) continue;
        if (!acc.canViewSharedThreads) continue;
      }
      byId.set(t.id, { ...t, _own: !!isOwn, _host: true });
    }
    // Merge guest's own local worker/review threads (authoritative for live running/unread state)
    for (const lt of localThreads) {
      if (lt.kind === 'lead') continue;
      const existing = byId.get(lt.id);
      byId.set(lt.id, { ...(existing || {}), ...lt, user_id: me.user_id, _own: true, _local: true });
    }
    return [...byId.values()].sort((a, b) => (b.updated_at || 0) - (a.updated_at || 0));
  }

  async function loadLocal() {
    try {
      const [ts, full] = await Promise.all([
        get(`projects/${enc(linkId)}/threads`, { quiet: true }).catch(() => []),
        get(`projects/${enc(linkId)}`, { quiet: true }).catch(() => null),
      ]);
      localThreads = Array.isArray(ts) ? ts : [];
      localDecisions = full?.decisions || [];
      localClaims = full?.claims || [];
      draw();
    } catch {
      draw();
    }
  }

  function card(t) {
    const me = getMe();
    const ownerLbl = threadOwnerLabel(t, me, personName);
    return h('div', {
      class: 'tcard cut' + (t.unread ? ' unread' : '') + (t.running ? ' running' : ''),
      style: { '--ah': agentHue(t.agent) },
      tabindex: '0',
      onclick: (e) => !e.target.closest('button') && peek(t.id),
      onkeydown: (e) => e.key === 'Enter' && peek(t.id),
      onauxclick: (e) => e.button === 1 && t._own && openThread(t.id, { pop: true }),
      oncontextmenu: (e) => (e.preventDefault(), cardMenu(t, e.clientX, e.clientY)),
    },
    h('div', { class: 'tc-top' },
      stamp(t.agent || 'guest', { model: t.model, small: true, lead: t.kind === 'lead' }),
      h('span', { class: 'tag tc-owner' + (t._own ? ' acc' : '') }, ownerLbl),
      t.kind === 'review' ? h('span', { class: 'tag' }, 'review') : null,
      h('span', { class: 'grow' }),
      statusChip(t)),
    h('h4', {}, t.title),
    t.status_note ? h('p', { class: 'tc-note' }, t.status_note) : t.last_body ? h('p', { class: 'tc-last' }, t.last_body.replace(/[#*_`>]/g, '').slice(0, 160)) : null,
    h('div', { class: 'tc-foot mono' },
      t.branch ? h('span', { title: t.branch }, ic(t.merged ? 'merge' : 'branch', 11), t.merged ? 'merged' : t.branch.replace(/^roamer\//, '')) : null,
      h('span', { class: 'grow' }),
      timeAgo(t.updated_at),
      t._own ? h('button', { class: 'ibtn xs', title: 'Pop out', onclick: () => openThread(t.id, { pop: true }), html: icon('popout', 12) }) : null));
  }

  function cardMenu(t, x, y) {
    const acc = getAccess();
    if (t._own) {
      const a = (action) => post(`threads/${t.id}/action`, { action }).then(loadLocal).catch((e) => toast(e.message, { err: true }));
      showMenu([
        { label: 'Open beside your chat', icon: 'thread', run: () => peek(t.id) },
        { label: 'Open full view', icon: 'fullscreen', run: () => go('#/t/' + t.id) },
        { label: 'Open in its own window', icon: 'popout', run: () => openThread(t.id, { pop: true }) },
        { sep: true },
        t.running ? { label: 'Pause', icon: 'pause', run: () => a('pause') } : null,
        t.running ? { label: 'Stop', icon: 'stop', run: () => a('stop') } : null,
        ['paused', 'limited', 'cancelled'].includes(t.status) && !t.running ? { label: 'Resume', icon: 'play', run: () => a('resume') } : null,
        t.status === 'failed' ? { label: 'Retry', icon: 'refresh', run: () => a('retry') } : null,
        { label: t.resolved ? 'Reopen' : 'Resolve', icon: t.resolved ? 'undo' : 'check', run: () => a(t.resolved ? 'reopen' : 'resolve') },
        { sep: true },
        { label: 'Delete', icon: 'trash', danger: true, run: async () => {
          await del('threads/' + t.id);
          loadLocal();
          toast('Thread deleted', { action: 'Undo', ms: 9000, run: () => post(`threads/${t.id}/restore`).then(loadLocal) });
        } },
      ].filter(Boolean), x, y);
      return;
    }
    showMenu([
      { label: 'Open beside your chat', icon: 'thread', run: () => peek(t.id) },
      acc.canMerge && t.branch && !t.merged ? { label: 'Merge branch on host…', icon: 'merge', run: () => mergeHostThread(t) } : null,
    ].filter(Boolean), x, y);
  }

  async function mergeHostThread(t) {
    const ok = await dialog({ title: `Merge "${t.title}"?`, text: `Merges branch ${t.branch} into the host’s base branch.`, ok: 'Merge' });
    if (!ok) return;
    try {
      const res = await post(base + `threads/${enc(t.id)}/merge`, {});
      toast(`Merged (${String(res.sha || '').slice(0, 7)})`);
    } catch (e) {
      toast(e.message, { err: true });
    }
  }

  function draw() {
    const acc = getAccess();
    const list = mergedThreads();
    const showRight = acc.canSeeThreads || list.length > 0;
    split.classList.toggle('board-chat-only', !showRight);
    boardEl.hidden = !showRight || !!peeked;
    if (!showRight) {
      split.dataset.pane = 'lead';
      mobile.onBadge?.({ needsYou: 0, unread: 0, extraUnread: 0 });
      return;
    }

    decisionsEl.replaceChildren(...localDecisions.map((d) => h('div', { class: 'b-dec' },
      h('button', { class: 'linkbtn small', onclick: () => peek(d.thread_id) }, 'from ' + (list.find((t) => t.id === d.thread_id)?.title || 'your thread')),
      decisionCard(d, (i) => post('decisions/' + d.id, { choice: i }).then(loadLocal).catch((e) => toast(e.message, { err: true }))))));

    if (!list.length) {
      lanes.replaceChildren(empty(
        'thread',
        'No threads yet',
        acc.canCreateThread
          ? 'Tell your lead what you want in your project chat, or open a thread yourself (it runs on your machine and syncs to the project).'
          : 'No shared threads to display yet.',
        acc.canCreateThread ? h('button', { class: 'btn', onclick: () => newThreadDialog(linkId) }, ic('plus', 13), 'New thread') : null,
      ));
    } else {
      lanes.replaceChildren(...LANES.map(([k, label, test]) => {
        const items = list.filter(test);
        if (!items.length && k !== 'work') return null;
        const folds = k === 'done' || k === 'fin';
        const collapsed = folds && !(k === 'done' ? doneOpen : finOpen);
        return h('section', { class: 'lane l-' + k },
          h('button', {
            class: 'lane-h',
            onclick: folds ? () => {
              if (k === 'fin') finOpen = !finOpen;
              else doneOpen = !doneOpen;
              try { localStorage.setItem(`mr-${k}-` + linkId, (k === 'fin' ? finOpen : doneOpen) ? '1' : ''); } catch { /* fine */ }
              draw();
            } : null,
          }, h('span', {}, label), h('b', { class: 'count' + (k === 'attn' ? ' hot' : '') }, items.length), folds ? h('i', { class: 'lane-chev' + (collapsed ? '' : ' open'), html: icon('chevDown', 12) }) : null),
          collapsed ? null : h('div', { class: 'lane-cards' }, items.length ? items.map(card) : h('p', { class: 'faint small lane-empty' }, 'Nothing running right now.')));
      }));
    }

    claimsEl.replaceChildren(...(localClaims.length ? [
      kicker('Your claimed files', h('small', { class: 'faint' }, 'shared with the team via presence')),
      h('div', { class: 'claims' }, localClaims.map((c) => h('span', { class: 'claim', style: { '--ah': agentHue(c.agent) }, title: `${c.title}${c.note ? ' — ' + c.note : ''}` }, h('code', {}, c.path), h('small', {}, agentName(c.agent))))),
    ] : []));

    const attnList = list.filter(LANES[0][2]);
    const extraDecs = localDecisions.filter((d) => !attnList.some((t) => t.id === d.thread_id)).length;
    const unread = list.filter((t) => t.unread).length;
    const extraUnread = list.filter((t) => t.unread && !attnList.includes(t)).length;
    mobile.onBadge?.({ needsYou: attnList.length + extraDecs, unread, extraUnread });
  }

  // Peek pane: own threads open with full local mountThread; others' host threads open with live host viewer + permission-gated composer.
  const peek = (id) => go('#/p/' + enc(linkId) + '?t=' + enc(id));
  let peeked = null;
  let stopPeek = null;
  let hostPeekPainter = null;

  mobile.onPane?.((next) => {
    split.dataset.pane = next;
    if (peeked) go('#/p/' + enc(linkId));
  });

  async function showPeek(id) {
    if (id === peeked) return;
    stopPeek?.();
    stopPeek = null;
    hostPeekPainter = null;
    peeked = id;
    const showRight = getAccess().canSeeThreads || mergedThreads().length > 0;
    boardEl.hidden = !showRight || !!id;
    peekEl.hidden = !id;
    split.classList.toggle('peeking', !!id);
    if (!id) return peekEl.replaceChildren();

    const list = mergedThreads();
    const t = list.find((x) => x.id === id) || hostThreads.get(id);
    const isOwn = t?._own || localThreads.some((x) => x.id === id);

    if (isOwn) {
      const title = h('b', { class: 'bp-title' }, t?.title || 'Thread');
      const pbody = h('div', { class: 'bp-body' });
      peekEl.replaceChildren(
        h('div', { class: 'b-lead-h bp-h' },
          h('button', { class: 'ibtn sm', title: 'Back to the board (Alt+Left)', onclick: () => back('#/p/' + enc(linkId)), html: icon('back', 14) }),
          title,
          h('span', { class: 'grow' }),
          h('button', { class: 'ibtn sm', title: 'Expand to the full view', onclick: () => go('#/t/' + id), html: icon('fullscreen', 14) }),
          h('button', { class: 'ibtn sm', title: 'Pop out', onclick: () => popOut('#/t/' + id, { key: 't:' + id }), html: icon('popout', 14) }),
          h('button', { class: 'ibtn sm', title: 'Close', onclick: () => back('#/p/' + enc(linkId)), html: icon('close', 14) })),
        pbody,
      );
      const stop = await mountThread(pbody, id, { compact: true, ctx: { setPopTitle: (nt) => (title.textContent = nt) } });
      if (peeked !== id) return stop?.();
      stopPeek = stop;
      return;
    }

    // Host thread (read-only unless message_threads / message_lead is permitted)
    if (!t || !getAccess().canViewSharedThreads) {
      peekEl.replaceChildren(
        h('div', { class: 'b-lead-h bp-h' },
          h('button', { class: 'ibtn sm', title: 'Back to the board', onclick: () => back('#/p/' + enc(linkId)), html: icon('back', 14) }),
          h('b', { class: 'bp-title' }, 'Thread unavailable'),
          h('span', { class: 'grow' }),
          h('button', { class: 'ibtn sm', title: 'Close', onclick: () => back('#/p/' + enc(linkId)), html: icon('close', 14) })),
        h('div', { class: 'bp-body bp-host-body' }, empty('shield', 'Thread unavailable', 'You don’t have permission to view this thread, or it was deleted.')),
      );
      return;
    }

    const msgBox = h('div', { class: 'collab-msgs' }, loading());
    const composeHost = h('div', { class: 'bp-host-compose' });
    const ownerLbl = threadOwnerLabel(t, getMe(), personName);

    function drawCompose() {
      const acc = getAccess();
      if (!acc.canMessageThread(t)) {
        composeHost.replaceChildren();
        return;
      }
      const text = textarea('', { rows: 2, placeholder: t.kind === 'lead' ? 'Write to the owner’s lead (uses their plan)' : 'Write to this thread (uses the owner’s plan)' });
      const send = h('button', {
        class: 'btn primary sm',
        onclick: async () => {
          if (!text.value.trim()) return;
          send.disabled = true;
          try {
            const m = await post(base + `threads/${enc(id)}/messages`, { text: text.value });
            if (!hostMsgs.has(id)) hostMsgs.set(id, new Map());
            hostMsgs.get(id).set(m.id, m);
            text.value = '';
            paintMsgs();
          } catch (e) {
            toast(e.message, { err: true });
          } finally {
            send.disabled = false;
          }
        },
      }, ic('send', 13), 'Send');
      text.addEventListener('keydown', (e) => {
        if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
          e.preventDefault();
          send.click();
        }
      });
      composeHost.replaceChildren(h('div', { class: 'collab-compose' }, text, send));
    }

    function paintMsgs() {
      const rows = [...(hostMsgs.get(id)?.values() || [])].sort((a, b) => a.created_at - b.created_at);
      msgBox.replaceChildren(...(rows.length ? rows.map((m) => h('div', { class: 'collab-msg ' + m.role },
        h('div', { class: 'collab-msg-h' }, h('b', {}, m.author || m.role), h('small', { class: 'faint' }, ago(m.created_at))),
        h('div', { class: 'md', html: md(m.body || '') }))) : [h('p', { class: 'faint small' }, 'No messages yet.')]));
      msgBox.scrollTop = msgBox.scrollHeight;
    }

    peekEl.replaceChildren(
      h('div', { class: 'b-lead-h bp-h' },
        h('button', { class: 'ibtn sm', title: 'Back to the board (Alt+Left)', onclick: () => back('#/p/' + enc(linkId)), html: icon('back', 14) }),
        h('b', { class: 'bp-title' }, t.title),
        h('span', { class: 'tag tc-owner' }, ownerLbl),
        statusChip(t),
        h('span', { class: 'grow' }),
        getAccess().canMerge && t.branch && !t.merged ? h('button', { class: 'btn xs', onclick: () => mergeHostThread(t) }, ic('merge', 12), 'Merge') : null,
        h('button', { class: 'ibtn sm', title: 'Close', onclick: () => back('#/p/' + enc(linkId)), html: icon('close', 14) })),
      h('div', { class: 'bp-body bp-host-body' },
        t.instructions ? h('details', { class: 'adv' }, h('summary', {}, 'Instructions'), h('div', { class: 'md', html: md(t.instructions) })) : null,
        msgBox,
        composeHost),
    );
    drawCompose();
    hostPeekPainter = ({ permsChanged }) => {
      if (permsChanged) {
        if (!getAccess().canViewSharedThreads) return back('#/p/' + enc(linkId));
        drawCompose();
      }
      paintMsgs();
    };

    try {
      const res = await get(base + `threads/${enc(id)}/messages?limit=120`);
      if (peeked !== id) return;
      if (!hostMsgs.has(id)) hostMsgs.set(id, new Map());
      (res.messages || []).forEach((m) => hostMsgs.get(id).set(m.id, m));
      paintMsgs();
    } catch (e) {
      if (peeked === id) msgBox.replaceChildren(errorBox(e, () => showPeek(id)));
    }
  }

  ctx?.onRoute?.((nr) => showPeek(nr.q.t || null));
  await loadLocal();
  await showPeek(r.q?.t || null);

  let timer = 0;
  const soon = () => (clearTimeout(timer), (timer = setTimeout(loadLocal, 300)));
  const unlisten = listen((ev) => {
    if (ev.kind === 'resync') return loadLocal();
    if (ev.project !== linkId && !(ev.kind === 'running' && localThreads.some((t) => t.id === ev.thread))) return;
    if (['thread', 'turn', 'running', 'decision', 'claims', 'message', 'progress'].includes(ev.kind)) soon();
  });
  const unlive = onLiveChange((ev) => {
    if (ev.threadsChanged || ev.permsChanged) draw();
    if (peeked && hostPeekPainter && (ev.permsChanged || ev.messages?.some((m) => m.thread_id === peeked))) {
      hostPeekPainter(ev);
    }
  });
  return () => (clearTimeout(timer), unlisten(), unlive(), stopPeek?.(), chatHandle.stop());
}

// ================================================================================================ collaborator settings tab
async function collabSettingsTab(el, collabCtx) {
  const { linkId, link, base, getAccess, getMe, getPeople, getPresence, getCatalog, getLive, getHostName, getProjectName, onLiveChange, redrawHead, redrawCoordination } = collabCtx;
  const wrap = h('div', { class: 'p-settings' });
  el.replaceChildren(wrap);

  function draw() {
    const acc = getAccess();
    const me = getMe();
    const live = getLive();
    const people = getPeople();
    const presence = getPresence();
    const catalog = getCatalog();
    const prefs = collabPrefs(linkId);
    const online = new Set((presence?.people || []).filter((p) => p.online).map((p) => p.user_id));

    const askOp = () => {
      if (!acc.canAskOperator) return null;
      if (me.requested_role) {
        return h('div', { class: 'collab-ask cut' }, ic('flag', 14), h('span', {}, 'Your request to become an operator has been sent to ', h('b', {}, getHostName()), '.'));
      }
      return h('div', { class: 'collab-ask cut' },
        ic('flag', 14),
        h('span', {}, 'Need to take threads over without asking each time? Ask the owner once to make you an operator.'),
        h('button', {
          class: 'btn sm primary',
          type: 'button',
          onclick: async () => {
            if (!(await dialog({ title: 'Ask to become an operator?', text: `${getHostName()} decides once. As an operator you can take threads over from them or others without asking each time.`, ok: 'Ask' }))) return;
            try {
              await post(base + 'request-role', { role: 'operator' });
              me.requested_role = 'operator';
              toast('Asked. You’ll see your role change here once they answer.');
              draw();
            } catch (e) {
              toast(e.message, { err: true });
            }
          },
        }, ic('flag', 12), 'Ask to be an operator'));
    };

    const permRows = (catalog.length ? catalog : Object.keys(acc.perms).map((id) => ({ id, label: id }))).map((p) => {
      const allowed = !!acc.perms[p.id];
      return h('div', { class: 'collab-perm-ro' },
        h('span', {}, p.label),
        h('span', { class: 'tag' + (allowed ? ' acc' : '') }, allowed ? 'Allowed' : 'Hidden'));
    });

    wrap.replaceChildren(
      h('section', { class: 'panel cut' },
        kicker('Your membership', h('span', { class: 'tag acc' }, ROLE_LABEL[acc.role] || acc.role)),
        h('dl', { class: 'facts' },
          h('dt', {}, 'Project'), h('dd', {}, getProjectName()),
          h('dt', {}, 'Hosted by'), h('dd', {}, getHostName()),
          h('dt', {}, 'Signed in as'), h('dd', {}, me.name || link.name),
          h('dt', {}, 'Status'), h('dd', {}, live === 'ok' ? h('span', { class: 'status live' }, h('i', { class: 'pulse' }), 'Live') : h('span', { class: 'status dim' }, 'Offline, retrying')),
        ),
        link.insecure ? h('div', { class: 'collab-warn' }, ic('warn', 14), h('span', {}, 'Plain http over the internet: ask the owner for a Tailscale or https address.')) : null,
        askOp(),
      ),
      h('section', { class: 'panel cut' },
        kicker('Effective permissions', h('small', { class: 'faint' }, 'set by the project owner (read-only)')),
        h('div', { class: 'collab-perm-ro-list' }, permRows),
      ),
      h('section', { class: 'panel cut' },
        kicker('People in this project', h('b', { class: 'count' }, people.length)),
        people.length
          ? h('div', { class: 'dev-list' }, people.map((p) => h('div', { class: 'dev' },
            acc.canViewPresence ? presenceDot(live === 'ok' && online.has(p.user_id)) : ic('people', 15),
            h('div', {},
              h('b', {}, p.name, p.user_id === me.user_id ? h('span', { class: 'tag' }, 'you') : null),
              h('small', { class: 'faint' }, (ROLE_LABEL[p.role] || p.role) + (p.last_seen ? ' · seen ' + ago(p.last_seen) : ''))))))
          : h('p', { class: 'faint small' }, 'People list will appear once connected to the host.'),
      ),
      h('section', { class: 'panel cut' },
        kicker('Notification & presence preferences', h('small', { class: 'faint' }, 'saved on this machine')),
        acc.canViewPresence ? toggle(prefs.show_presence, (v) => {
          setCollabPref(linkId, { show_presence: v });
          redrawCoordination();
        }, { label: 'Show the presence toolbar on this project' }) : null,
        toggle(prefs.notify_notes, (v) => setCollabPref(linkId, { notify_notes: v }), { label: 'Highlight conflict notes in the status bar' }),
        toggle(prefs.notify_updates, (v) => setCollabPref(linkId, { notify_updates: v }), { label: 'Show a toast when your role or permissions change' }),
        h('p', { class: 'faint small' }, ic('info', 12), acc.canSharePresence
          ? ' Your active threads and file claims in this project are shared with the team via presence heartbeats.'
          : ' Sharing your presence is turned off by the owner’s rules for your role.'),
      ),
      h('section', { class: 'panel cut' },
        kicker('Leave project'),
        h('p', { class: 'faint small' }, `Leaving removes ${getProjectName()} from your sidebar and tells ${getHostName()}. To rejoin later you will need a new invite link.`),
        h('div', { class: 'row' },
          h('button', { class: 'btn danger', type: 'button', onclick: () => leaveJoinedProject({ ...link, project_name: getProjectName(), host_name: getHostName() }) }, ic('logout', 13), 'Leave project')),
      ),
    );
    redrawHead();
  }

  draw();
  return onLiveChange(() => draw());
}

// ================================================================================================ owner board
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
    const pbody = h('div', { class: 'bp-body' });
    peekEl.replaceChildren(h('div', { class: 'b-lead-h bp-h' },
      h('button', { class: 'ibtn sm', title: 'Back to the board (Alt+Left)', onclick: () => back('#/p/' + p.id), html: icon('back', 14) }),
      title,
      h('span', { class: 'grow' }),
      h('button', { class: 'ibtn sm', title: 'Expand to the full view', onclick: () => go('#/t/' + id), html: icon('fullscreen', 14) }),
      h('button', { class: 'ibtn sm', title: 'Pop out', onclick: () => popOut('#/t/' + id, { key: 't:' + id }), html: icon('popout', 14) }),
      h('button', { class: 'ibtn sm', title: 'Close', onclick: () => back('#/p/' + p.id), html: icon('close', 14) })), pbody);
    const stop = await mountThread(pbody, id, { compact: true, ctx: { setPopTitle: (t) => (title.textContent = t) } });
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
  return () => (clearTimeout(timer), unlisten(), stopPeek?.(), stopLead?.());
}

// ------------------------------------------------------------ new thread (by hand)
export async function newThreadDialog(projectId, { repo = '' } = {}) {
  const access = accessFromStore(projectId);
  if (access.isJoined && !access.canCreateThread) {
    toast('The project’s owner hasn’t allowed you to open threads here.', { err: true });
    return;
  }
  if (access.isJoined) {
    try {
      await ensureLocal({ id: projectId });
    } catch (e) {
      toast(e.message, { err: true });
      return;
    }
  }
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
  const matches = h('div');
  let reviewedTitle = '';
  title.addEventListener('input', () => { matches.replaceChildren(); reviewedTitle = ''; });
  const ok = await dialog({
    title: 'New thread',
    text: access.isJoined
      ? 'Opens a thread you own on this machine and syncs it to the shared project.'
      : 'Opens a worker thread yourself. The lead hears about it and plans around it.',
    wide: true,
    body: h('div', { class: 'dlg-fields' }, field('Title', title), matches, field('Instructions', instr), field('Done when', crit),
      h('div', { class: 'row2' }, field('Kind of work', kinds, 'Routing picks the agent for the kind.'), field('Agent', pick.el)),
      repoPick ? field('Repository', repoPick, 'Which of the project’s repositories this thread works on. Its branch, merge and pull request go there.') : null,
      field('Workspace', ws, 'Own branch: a git worktree, merged when you say so. Project folder: works in place (good for research and reading).')),
    ok: 'Start thread',
    check: async () => {
      if (!title.value.trim()) return 'Give it a title.';
      if (!instr.value.trim()) return 'Say what the agent should do.';
      const { agent: a, model: m, effort: ef } = pick.get();
      try {
        const proposed = title.value.trim();
        if (reviewedTitle !== proposed) {
          const overlap = await ownerOverlap(projectId, proposed);
          if (overlap) {
            matches.replaceChildren(overlap);
            reviewedTitle = proposed;
            return 'Review the overlap. Choose Start thread again to continue.';
          }
        }
        made = await post(`projects/${projectId}/workers`, { title: title.value.trim(), instructions: instr.value, criteria: crit.value, task_kind: kinds.value, agent: a, model: m, effort: ef, workspace: wsv, repo: repoV });
      } catch (e) {
        return e.message;
      }
    },
  });
  if (ok && made) toast(`Started ${made.title} with ${agentName(made.agent)}`, { action: 'Open', run: () => go('#/p/' + projectId + '?t=' + made.id) });
}

// ------------------------------------------------------------ git (owner + permitted joined project)
async function gitTab(el, p, repo = '', opts = {}) {
  const collab = opts.collab || null;
  const apiBase = collab ? collab.viewBase : `projects/${p.id}/`;
  const canMerge = () => (!collab || collab.getAccess().canMerge);

  async function renderGit() {
    el.replaceChildren(loading('Reading the repository'));
    const rq = repo ? '?repo=' + encodeURIComponent(repo) : '';
    const [g, prs] = await Promise.all([
      get(`${apiBase}git${rq}`).catch((e) => ({ error: e.message })),
      get(`${apiBase}prs${rq}`).catch((e) => ({ error: e.message })),
    ]);
    const pickRepo = repoSelect(p.repos, repo, (v) => go(`#/p/${p.id}/git` + (v && v !== 'primary' ? '?repo=' + encodeURIComponent(v) : '')));
    const top = pickRepo ? h('div', { class: 'repo-bar' }, h('span', { class: 'eyebrow' }, 'Repository'), pickRepo) : null;
    if (g.error) return el.replaceChildren(...[top, errorBox({ message: g.error }, renderGit)].filter(Boolean));
    if (!g.repo) {
      return el.replaceChildren(...[top].filter(Boolean), empty('branch', 'Not a git repository', g.git ? `${g.folder} has no git history. Agents still work there, but in place (no branches, no merging).` : 'Git isn’t installed on this machine. Install it under Agents › Tools.', g.git ? null : h('a', { class: 'btn', href: '#/agents?tab=tools' }, 'Install Git')));
    }
    const changes = (g.status || '').split('\n').filter((l) => l && !l.startsWith('##'));
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
              canMerge() && r.state === 'open' && !r.draft ? h('button', { class: 'btn xs', onclick: (e) => (e.preventDefault(), mergePr(apiBase, r, renderGit, repo)) }, 'Merge') : null)))
              : h('p', { class: 'faint small' }, 'No pull requests yet.'))));
  }

  await renderGit();
  return collab?.onLiveChange?.(({ permsChanged }) => permsChanged && renderGit()) || (() => {});
}

async function mergePr(apiBase, r, after, repo = '') {
  const method = select([['merge', 'Merge commit'], ['squash', 'Squash into one commit'], ['rebase', 'Rebase onto ' + (r.base || 'the base')]], 'merge');
  const okd = await dialog({ title: `Merge pull request #${r.number}?`, text: `Merges ${r.branch} into ${r.base} on GitHub. GitHub refuses if checks or reviews it requires haven't passed.`, body: field('How', method), ok: 'Merge' });
  if (!okd) return;
  try {
    await post(`${apiBase}prs/${r.number}/merge`, { method: method.value, repo });
    toast(`Merged #${r.number}`, { action: 'View', run: () => openExternal(r.url) });
    after();
  } catch (e) {
    toast('Merge failed: ' + e.message, { err: true, ms: 9000 });
  }
}

// ------------------------------------------------------------ owner settings (+ People & Collaborators)
async function settingsTab(el, p, onSaved, r) {
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

  const peopleSection = h('div', { class: 'p-people-section', id: 'project-people' });
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
    h('div', { class: 'save-bar' }, h('button', { class: 'btn primary', onclick: save }, 'Save changes'), h('span', { class: 'grow' }), h('button', { class: 'btn danger', onclick: () => deleteProject(p) }, ic('trash', 13), 'Delete project')),
    h('div', { class: 'p-people-head' }, kicker('People & Collaborators', h('a', { class: 'chipbtn', href: '#/settings/collab?project=' + enc(p.id) }, 'All projects overview'))),
    peopleSection));

  const { projectPeoplePanel } = await import('./collab.js');
  const stopPeople = await projectPeoplePanel(peopleSection, p.id);
  if (r?.q?.section === 'people') {
    requestAnimationFrame(() => peopleSection.scrollIntoView({ behavior: 'smooth', block: 'start' }));
  }
  return () => stopPeople?.();
}

// ------------------------------------------------------------ repositories
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
  const unlisten = listen((ev) => {
    if (!box.isConnected) return ((dead = true), unlisten());
    if (ev.kind === 'repos' && ev.project === p.id && !adding) load();
  });
  return box;
}

export { projectOf, ago, modelOptions };
