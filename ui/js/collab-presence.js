import { get, listen, post } from './api.js';
import { field, ic, kicker, textarea } from './common.js';
import { closePopover, popover, toast } from './ui.js';
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

const controls = new WeakMap();

function drawControl(host, { key, label, summary, content, open = false, memory }) {
  let control = controls.get(host);
  if (!control) {
    const body = h('div', { class: 'collab-pop-body', tabindex: '-1', role: 'region', 'aria-label': label });
    const button = h('button', { type: 'button', class: 'collab-strip', 'aria-expanded': 'false' });
    control = { button, body, popup: null, key: null, remember: true };
    controls.set(host, control);
    const store = (expanded) => {
      if (memory && control.remember) {
        try { localStorage.setItem(memory, expanded ? 'open' : 'closed'); } catch { /* private window */ }
      }
    };
    control.open = (focus) => {
      if (control.popup || !button.isConnected) return;
      control.popup = popover(body, { anchor: button, cls: 'collab-pop', onClose: () => {
        control.popup = null;
        button.setAttribute('aria-expanded', 'false');
        store(false);
        if (document.activeElement === document.body && button.isConnected) button.focus({ preventScroll: true });
      } });
      button.setAttribute('aria-expanded', 'true');
      const top = button.getBoundingClientRect().bottom + 6;
      if (top + control.popup.offsetHeight > innerHeight - 6) {
        control.popup.style.top = Math.max(6, innerHeight - control.popup.offsetHeight - 6) + 'px';
      }
      store(true);
      if (focus) body.focus({ preventScroll: true });
    };
    button.addEventListener('click', () => control.popup ? closePopover() : control.open(true));
    host.replaceChildren(button);
    let saved = false;
    try { saved = !!memory && localStorage.getItem(memory) === 'open'; } catch { /* private window */ }
    if (open || (saved && !requestedInbox())) requestAnimationFrame(() => control.open(open));
  }
  if (control.key === key) return;
  control.key = key;
  control.button.setAttribute('aria-label', label);
  control.button.replaceChildren(...summary);
  control.body.replaceChildren(...content);
}

export function clearControl(host) {
  const control = controls.get(host);
  if (control?.popup) {
    control.remember = false;
    closePopover();
  }
  controls.delete(host);
  host.replaceChildren();
}

export function drawPresence(host, view, live = true, project = '') {
  if (!view?.permission || !view.people?.length) return clearControl(host);
  const online = live ? view.people.filter((p) => p.online) : [];
  const workers = online.filter((p) => p.work.length).length;
  const count = live ? (workers ? `${workers} working` : `${online.length} online`) : 'Offline';
  const summary = [
    ic('people', 14), h('span', {}, 'Presence'),
    h('span', { class: 'collab-strip-dots', 'aria-hidden': 'true' }, view.people.slice(0, 4).map((p) => presenceDot(live && p.online))),
    h('b', { class: 'collab-strip-count' }, count),
    ic('down', 12),
  ];
  drawControl(host, { key: JSON.stringify([view, live]), label: 'Who is working on what · ' + count, summary,
    memory: project ? 'mr-presence-' + project : null,
    content: [kicker('Who is working on what'),
      h('div', { class: 'collab-work-list' }, view.people.map((p) =>
      h('div', { class: 'collab-work-person' },
        h('div', { class: 'collab-work-person-head' }, presenceDot(live && p.online), h('b', {}, p.name), h('span', { class: 'tag' }, p.role)),
        live && p.online ? (p.work.length ? p.work.map(workLine) : h('small', { class: 'faint' }, 'No active work')) : null)))],
  });
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

export async function checkWork(base, work, mayNote = true, notesBase = base) {
  try {
    const matches = await post(base + 'overlaps', work);
    return overlapNotices(notesBase, matches, mayNote);
  } catch (e) {
    if (e.status === 403 || e.status === 404) return null;
    throw e;
  }
}

export function drawInbox(host, base, notes, refresh, open = false) {
  const pending = (notes || []).filter((n) => !n.decision);
  if (!pending.length) return clearControl(host);
  const need = pending.filter((n) => n.needs_you).length;
  const content = [kicker('Conflict notes'), ...pending.map((note) => {
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
    })];
  drawControl(host, { key: JSON.stringify(pending), label: `Conflict notes inbox · ${need} need you`, open, content,
    summary: [ic('flag', 14), h('span', {}, 'Inbox'), h('b', { class: 'collab-strip-count' + (need ? ' hot' : '') }, need || pending.length)],
  });
}

export const requestedInbox = () => new URLSearchParams(location.hash.split('?')[1]).has('inbox');

export function mountProjectInbox(host, project) {
  const base = endpoint(project);
  let alive = true;
  let request = 0;
  let open = requestedInbox();
  const refresh = async () => {
    const current = ++request;
    try {
      const notes = await get(base + 'notes', { quiet: true });
      if (!alive || current !== request) return;
      drawInbox(host, base, notes, refresh, open);
      if (notes.length) open = false;
    } catch { /* keep the last known inbox until reconnect */ }
  };
  const unlisten = listen((ev) => {
    if ((ev.project === project && ['conflict-notes', 'collab'].includes(ev.kind)) || ev.kind === 'resync') refresh();
  });
  refresh();
  return {
    open: () => {
      const control = controls.get(host);
      if (control) control.open(true);
      else open = true;
    },
    destroy: () => { alive = false; unlisten(); clearControl(host); },
  };
}

export function mountPresence(host, project) {
  const base = endpoint(project);
  let alive = true;
  let request = 0;
  const refresh = async () => {
    const current = ++request;
    try {
      const view = await get(base + 'presence', { quiet: true });
      if (alive && current === request) drawPresence(host, view, true, project);
    } catch { /* keep the previous panel until the local server reconnects */ }
  };
  const unlisten = listen((ev) => {
    if ((ev.project === project && ['presence', 'collab'].includes(ev.kind)) || ev.kind === 'resync') refresh();
  });
  refresh();
  return () => { alive = false; unlisten(); clearControl(host); };
}

export function mountCoordination(host, project, thread, kind, withInbox) {
  const base = endpoint(project);
  let alive = true;
  let request = 0;
  let rendered = '';
  const inbox = h('div', { class: 'collab-note-host' });
  const overlaps = h('div');
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
      const notices = overlapNotices(actionBase, data.overlaps, data.may_note);
      overlaps.replaceChildren(notices);
      if (withInbox) drawInbox(inbox, actionBase, notes, refresh);
      host.replaceChildren(...[notices ? overlaps : null, inbox.childElementCount ? inbox : null].filter(Boolean));
    } catch { /* keep the last known inbox until reconnect */ }
  };
  const unlisten = listen((ev) => {
    if ((ev.project === project && ['presence', 'conflict-notes', 'claims', 'collab'].includes(ev.kind)) || ev.kind === 'resync') refresh();
  });
  refresh();
  return () => { alive = false; unlisten(); clearControl(inbox); };
}

export function ownerOverlap(baseProject, title) {
  const base = endpoint(baseProject);
  const notesBase = baseProject.startsWith('lnk_') ? `collab/links/${encodeURIComponent(baseProject)}/host/` : base;
  return checkWork(base, { title, kind: 'worker' }, true, notesBase);
}
