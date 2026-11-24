import { get, listen, post } from './api.js';
import { field, ic, kicker, textarea } from './common.js';
import { toast } from './ui.js';
import { h } from './util.js';

const endpoint = (project) => `collab/projects/${encodeURIComponent(project)}/`;

export function presenceDot(online) {
  return h('span', { class: 'collab-presence-dot' + (online ? ' online' : ''), title: online ? 'Online' : 'Offline',
    role: 'img', 'aria-label': online ? 'Online' : 'Offline' });
}

function workLine(work) {
  return h('div', { class: 'collab-work' },
    h('b', {}, work.title),
    h('span', { class: 'tag' }, work.status),
    work.branch ? h('span', { class: 'collab-work-branch mono' }, ic('branch', 12), work.branch) : null,
    work.paths?.length ? h('ul', { class: 'collab-work-paths mono', 'aria-label': 'Claimed paths' },
      work.paths.map((path) => h('li', {}, path))) : null);
}

export function presencePanel(view, live = true) {
  if (!view?.permission || !view.people?.length) return null;
  const num = live ? view.people.filter((p) => p.online).length : 0;
  return h('details', { class: 'collab-work-panel' },
    h('summary', {}, ic('people', 15), h('span', {}, 'Who is working on what'),
      h('span', { class: 'tag' }, live ? `${num} online` : 'Offline')),
    h('div', { class: 'collab-work-list' }, view.people.map((p) =>
      h('div', { class: 'collab-work-person' },
        h('div', { class: 'collab-work-person-head' }, presenceDot(live && p.online), h('b', {}, p.name), h('span', { class: 'tag' }, p.role)),
        live && p.online ? (p.work.length ? p.work.map(workLine) : h('small', { class: 'faint' }, 'No active work')) : null))));
}

const presenceKeys = new WeakMap();

export function drawPresence(host, view, live = true) {
  const key = JSON.stringify([view, live]);
  if (presenceKeys.get(host) === key) return;
  presenceKeys.set(host, key);
  const open = host.firstElementChild?.open;
  const focused = host.contains(document.activeElement);
  const panel = presencePanel(view, live);
  if (panel) panel.open = open || false;
  host.replaceChildren(...(panel ? [panel] : []));
  if (focused) panel?.querySelector('summary')?.focus({ preventScroll: true });
}

function noteComposer(base, overlap) {
  const text = textarea('', { rows: 4, maxlength: 1600, placeholder: 'What would you like to coordinate?' });
  const detail = h('details', { class: 'collab-note-compose' });
  const send = h('button', { class: 'btn xs primary', type: 'button', onclick: async () => {
      if (!text.value.trim()) return text.focus();
      send.disabled = true;
      try {
        await post(base + 'notes', { recipient: overlap.user_id, thread: overlap.thread, text: text.value.trim() });
        text.value = '';
        detail.open = false;
        toast('Note sent for review');
      } catch (e) {
        toast(e.message, { err: true });
      } finally {
        send.disabled = false;
      }
    } }, ic('send', 12), 'Send for review');
  detail.append(
    h('summary', { class: 'btn xs' }, 'Send note'),
    field(`Note to ${overlap.name}`, text, 'The recipient can review it before it reaches their thread.'),
    send);
  return detail;
}

export function overlapNotices(base, matches, mayNote = true) {
  if (!matches?.length) return null;
  return h('section', { class: 'collab-notices', 'aria-label': 'Work overlaps' },
    kicker('Work overlaps'),
    matches.map((overlap) => h('div', { class: 'collab-notice' },
      h('div', {}, h('b', {}, `${overlap.name} · ${overlap.title}`),
        h('small', { class: overlap.path ? 'mono' : 'faint' }, overlap.path || 'Same task title')),
      mayNote ? noteComposer(base, overlap) : null)));
}

export async function checkWork(base, work, mayNote = true) {
  try {
    const matches = await post(base + 'overlaps', work);
    return overlapNotices(base, matches, mayNote);
  } catch (e) {
    if (e.status === 403 || e.status === 404) return null;
    throw e;
  }
}

export function notesInbox(base, notes, refresh) {
  const pending = (notes || []).filter((n) => !n.decision);
  if (!pending.length) return null;
  return h('section', { class: 'collab-notices', 'aria-label': 'Conflict notes inbox' },
    kicker('Conflict notes', h('span', { class: 'tag' }, pending.filter((n) => n.needs_you).length + ' need you')),
    pending.map((note) => {
      const decide = async (action) => {
        try {
          const result = await post(base + 'notes/' + encodeURIComponent(note.id), { action });
          refresh();
          toast(action === 'approve' ? 'Note approved' : 'Note declined', { action: 'Undo', ms: result.undo_ms,
            run: async () => {
              try {
                await post(base + 'notes/' + encodeURIComponent(note.id), { action: 'undo' });
                refresh();
                toast('Note back in the inbox');
              } catch (e) {
                toast(e.message, { err: true });
              }
            } });
        } catch (e) {
          toast(e.message, { err: true });
          refresh();
        }
      };
      return h('article', { class: 'collab-note' },
        h('b', {}, `From ${note.sender_name} · ${note.title}`),
        h('p', {}, note.body),
        h('div', { class: 'collab-note-actions' },
          h('button', { class: 'btn xs', type: 'button', onclick: () => decide('decline') }, 'Decline'),
          h('button', { class: 'btn primary xs', type: 'button', onclick: () => decide('approve') }, 'Approve')));
    }));
}

export function mountPresence(host, project) {
  const base = endpoint(project);
  let alive = true;
  let request = 0;
  const refresh = async () => {
    const current = ++request;
    try {
      const view = await get(base + 'presence', { quiet: true });
      if (alive && current === request) drawPresence(host, view);
    } catch { /* keep the previous panel until the local server reconnects */ }
  };
  const unlisten = listen((ev) => {
    if ((ev.project === project && ['presence', 'collab'].includes(ev.kind)) || ev.kind === 'resync') refresh();
  });
  refresh();
  return () => { alive = false; unlisten(); };
}

export function mountCoordination(host, project, thread, kind) {
  const base = endpoint(project);
  let alive = true;
  let request = 0;
  let rendered = '';
  const refresh = async () => {
    const current = ++request;
    try {
      const data = await get(base + 'coordination?thread=' + encodeURIComponent(thread), { quiet: true });
      if (!alive || current !== request) return;
      const notes = kind === 'lead' ? data.notes : data.notes.filter((note) => note.thread_id === thread && note.needs_you);
      const key = JSON.stringify([data.overlaps, notes, data.may_note, data.link_id]);
      if (key === rendered) return;
      rendered = key;
      const actionBase = data.link_id ? `collab/links/${encodeURIComponent(data.link_id)}/host/` : base;
      host.replaceChildren(...[overlapNotices(actionBase, data.overlaps, data.may_note), notesInbox(actionBase, notes, refresh)].filter(Boolean));
    } catch { /* keep the last known inbox until reconnect */ }
  };
  const unlisten = listen((ev) => {
    if ((ev.project === project && ['presence', 'conflict-notes', 'claims', 'collab'].includes(ev.kind)) || ev.kind === 'resync') refresh();
  });
  refresh();
  return () => { alive = false; unlisten(); };
}

export function ownerOverlap(baseProject, title) {
  return checkWork(endpoint(baseProject), { title, kind: 'worker' });
}
