// Per-person project chats (collaboration Stage 1c, see src/collab_chat.rs):
// - a guest's own project chat in a joined project: their own lead, running on this machine on their own agents,
//   with the host's project fed in as short summaries. It sits in the project page's left pane like the owner's lead.
// - the owner's switch for reading collaborators' chats, and who can read each one.

import { h, ago } from '../util.js';
import { icon } from '../icons.js';
import { get, post, patch, listen } from '../api.js';
import { go } from '../app.js';
import { popOut } from '../native.js';
import { toast } from '../ui.js';
import { mountThread } from './thread.js';
import { ic, stamp, toggle, loading, errorBox, kicker } from '../common.js';

const enc = encodeURIComponent;

/** Make sure this machine has the joined project's local side (the project and the person's chat). */
export function ensureLocal(link) {
  return post(`collab/links/${enc(link.id)}/chat`, {}, { timeout: 30000 });
}

function readersText(readers, host) {
  const names = (readers || []).map((x) => (x === 'owner' ? `${host} (the owner)` : x));
  return names.join(' and ');
}

function buildBar(bar, { readers, host, offline, threadId, showExpand }) {
  const shared = (readers || []).length > 0;
  bar.replaceChildren(
    h('span', {
      class: 'status ' + (shared ? 'acc' : 'dim'),
      title: shared ? 'The owner’s rules share this chat' : 'Nobody else can read this chat',
    }, ic(shared ? 'people' : 'lock', 12), shared ? 'Shared' : 'Private'),
    h('span', { class: 'cc-note' }, shared
      ? `${readersText(readers, host)} can read this chat (the owner’s rules). Your agents still run only here.`
      : 'Only you can read this chat: it stays on this machine. Your lead runs here on your own agents.'),
    offline ? h('span', { class: 'status dim', title: offline }, 'Host offline') : null,
    showExpand && threadId ? h('button', { class: 'ibtn sm', title: 'Open as a full thread', onclick: () => go('#/t/' + threadId) }, ic('fullscreen', 14)) : null,
  );
}

/**
 * Mount the guest's own personal project chat inside the project board's left pane (.b-lead).
 * Matches the owner's board header + adds the Private/Shared privacy bar.
 */
export async function mountBoardChat(leadBox, link, { ensured = null, leadAgent = '', leadModel = '' } = {}) {
  const bar = h('div', { class: 'cc-bar cc-bar-board' });
  const wrap = h('div', { class: 'cc-board-thread' }, loading('Opening your chat'));
  leadBox.replaceChildren(wrap);

  let r = ensured;
  if (!r?.thread?.id) {
    try {
      r = await ensureLocal(link);
    } catch (e) {
      wrap.replaceChildren(errorBox(e, () => mountBoardChat(leadBox, link, { leadAgent, leadModel })));
      return { stop: () => {}, setPrivacy: () => {} };
    }
  }
  const host = r.host?.name || link.host_name;
  const tid = r.thread.id;
  const agent = r.thread.agent || leadAgent || 'claude';
  const model = r.thread.model || leadModel || '';
  const head = h('div', { class: 'b-lead-h' },
    h('span', { class: 'eyebrow' }, 'Project chat'),
    stamp(agent, { model, small: true, lead: true }),
    h('span', { class: 'grow' }),
    h('button', { class: 'ibtn sm', title: 'Open as a full thread', onclick: () => go('#/t/' + tid), html: icon('fullscreen', 14) }),
    h('button', { class: 'ibtn sm', title: 'Pop out', onclick: () => popOut('#/t/' + tid, { key: 't:' + tid }), html: icon('popout', 14) }),
  );
  leadBox.replaceChildren(head, bar, wrap);
  let state = { readers: r.readers || [], offline: r.offline || '' };
  const paintBar = () => buildBar(bar, { readers: state.readers, host, offline: state.offline, threadId: tid, showExpand: false });
  paintBar();

  const stopThread = await mountThread(wrap, tid, { compact: true });
  const un = listen(async (ev) => {
    if (ev.kind !== 'collab-links' || ev.data?.id !== link.id || !('share_chat' in (ev.data || {}))) return;
    try {
      const fresh = await ensureLocal(link);
      state = { readers: fresh.readers || [], offline: fresh.offline || '' };
      paintBar();
    } catch { /* keep what we knew */ }
  });
  return {
    stop: () => {
      un();
      if (typeof stopThread === 'function') stopThread();
    },
    setPrivacy: ({ readers, offline } = {}) => {
      if (readers !== undefined) state.readers = readers;
      if (offline !== undefined) state.offline = offline;
      paintBar();
    },
  };
}

/** Legacy standalone mount (kept for compatibility if called directly). */
export async function mountChat(el, link) {
  const bar = h('div', { class: 'cc-bar' });
  const box = h('div', { class: 'cc-chat' }, loading('Opening your chat'));
  el.replaceChildren(kicker('Your project chat'), bar, box);
  let r;
  try {
    r = await ensureLocal(link);
  } catch (e) {
    box.replaceChildren(errorBox(e, () => mountChat(el, link)));
    return () => {};
  }
  const host = r.host?.name || link.host_name;
  const paintBar = () => buildBar(bar, { readers: r.readers, host, offline: r.offline, threadId: r.thread.id, showExpand: true });
  paintBar();
  const stop = await mountThread(box, r.thread.id, { compact: true });
  const un = listen(async (ev) => {
    if (ev.kind !== 'collab-links' || ev.data?.id !== link.id || !('share_chat' in (ev.data || {}))) return;
    try {
      r = { ...r, ...(await ensureLocal(link)) };
      paintBar();
    } catch { /* keep what we knew */ }
  });
  return () => {
    un();
    if (typeof stop === 'function') stop();
  };
}

/** Owner: may I read each person's own chat, and who can read whose. */
export async function chatsPanel(box, pid) {
  let d;
  try {
    d = await get(`collab/projects/${enc(pid)}/chats`);
  } catch (e) {
    return box.replaceChildren(errorBox(e, () => chatsPanel(box, pid)));
  }
  box.replaceChildren(kicker('Project chats', h('small', { class: 'faint' }, 'each person talks to their own lead')),
    h('div', { class: 'cc-row' },
      h('span', {}, h('b', {}, 'Let me read each person’s project chat'),
        h('small', { class: 'faint' }, d.owner
          ? 'On: their chats are copied here as they go, and you can open them below. They can see that you can.'
          : 'Off: each person’s chat stays on their own machine. Only its status shows here.')),
      toggle(d.owner, async (v) => {
        try {
          d = await patch(`collab/projects/${enc(pid)}/chats`, { owner: v });
          toast(v ? 'You can read their chats now' : 'Their chats are private again');
          chatsPanel(box, pid);
        } catch (e) {
          toast(e.message, { err: true });
        }
      }, { title: 'Read collaborators’ project chats' })),
    d.chats.length ? h('div', { class: 'dev-list' }, d.chats.map((c) => h('div', { class: 'dev' }, ic('chat', 15),
      h('div', {}, h('b', {}, `${c.name || 'Someone'}’s chat`),
        h('small', { class: 'faint' }, `${c.status}${c.updated_at ? ' · ' + ago(c.updated_at) : ''} · ${c.readers.length ? 'readable by ' + c.readers.map((x) => (x === 'owner' ? 'you' : x)).join(', ') : 'private to them'}`)),
      c.id ? h('button', { class: 'btn xs', onclick: () => go('#/t/' + c.id) }, 'Open') : h('span', { class: 'tag' }, ic('lock', 11), 'private')))) : h('p', { class: 'faint small' }, 'Nobody has opened their chat yet.'),
    h('p', { class: 'faint small' }, ic('shield', 12), ' Collaborators can be allowed to read each other’s chats with the rule “see other people’s own project chats” above.'));
}

/** Owner label for a thread card on a joined project board. */
export function threadOwnerLabel(t, me, personName) {
  if (me && t.user_id && t.user_id === me.user_id) return 'yours';
  if (t.kind === 'lead') return (personName('') || 'Owner') + '’s lead';
  if (t.kind === 'chat') return personName(t.user_id) + '’s chat';
  return t.user_id ? personName(t.user_id) : (personName('') || 'Owner');
}

/** Legacy helper kept for compatibility. */
export function threadTag(t, me, personName) {
  if (t.user_id === me.user_id) return ' · yours';
  if (t.kind === 'lead') return ' · ' + personName('') + '’s chat';
  if (t.kind === 'chat') return ' · ' + personName(t.user_id) + '’s chat';
  return t.user_id ? ' · ' + personName(t.user_id) : '';
}
