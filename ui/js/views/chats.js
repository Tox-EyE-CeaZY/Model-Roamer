// Chats: one-to-one conversations with any agent, outside a project (each gets a folder of its own).

import { h, ago } from '../util.js';
import { icon } from '../icons.js';
import { get, post, patch, del, listen } from '../api.js';
import { S, agentName } from '../store.js';
import { openThread, go } from '../app.js';
import { dialog, toast, showMenu } from '../ui.js';
import { ic, stamp, statusChip, empty, loading, errorBox, field, input, textarea, agentModelPicker, pickFolder } from '../common.js';

export default async function chats(el) {
  const list = h('div', { class: 'chat-list' });
  const q = input('', { placeholder: 'Filter chats', type: 'search' });
  el.append(
    h('div', { class: 'page-h' }, h('div', {}, h('h1', {}, 'Chats'), h('p', { class: 'lede' }, 'Talk to one agent directly, outside a project. Good for questions, quick scripts and trying a model.')),
      h('button', { class: 'btn primary', onclick: () => newChat() }, ic('plus', 14), 'New chat')),
    h('div', { class: 'toolbar' }, q),
    list);
  let all = [];
  async function load() {
    try {
      all = await get('threads?kind=chat');
      draw();
    } catch (e) {
      list.replaceChildren(errorBox(e, load));
    }
  }
  function draw() {
    const f = q.value.trim().toLowerCase();
    const shown = all.filter((t) => !f || t.title.toLowerCase().includes(f) || (t.last_body || '').toLowerCase().includes(f));
    if (!all.length) return list.replaceChildren(empty('chat', 'No chats yet', 'Start one with any agent you have set up. Ctrl+N works from anywhere.', h('button', { class: 'btn primary', onclick: () => newChat() }, 'New chat')));
    if (!shown.length) return list.replaceChildren(h('p', { class: 'faint' }, 'No chat matches that.'));
    list.replaceChildren(...shown.map((t) => h('div', {
      class: 'chat-row cut' + (t.unread ? ' unread' : ''),
      tabindex: '0',
      onclick: (e) => !e.target.closest('button') && openThread(t.id),
      onkeydown: (e) => e.key === 'Enter' && openThread(t.id),
      oncontextmenu: (e) => (e.preventDefault(), menu(t, e.clientX, e.clientY)),
    },
    t.pinned ? h('i', { class: 'pin', html: icon('pin', 13) }) : null,
    h('div', { class: 'cr-main' }, h('b', {}, t.title), h('p', {}, (t.last_body || '').replace(/[#*_`>]/g, '').slice(0, 180))),
    stamp(t.agent, { model: t.model, small: true }), statusChip(t), h('small', { class: 'faint' }, ago(t.updated_at)),
    h('button', { class: 'ibtn sm', title: 'More', onclick: (e) => menu(t, e.clientX, e.clientY), html: icon('more', 14) }))));
  }
  function menu(t, x, y) {
    showMenu([
      { label: 'Open', icon: 'chat', run: () => openThread(t.id, { pop: false }) },
      { label: 'Open in its own window', icon: 'popout', run: () => openThread(t.id, { pop: true }) },
      { label: t.pinned ? 'Unpin' : 'Pin to the top', icon: 'pin', run: () => patch('threads/' + t.id, { pinned: t.pinned ? 0 : 1 }).then(load) },
      { sep: true },
      { label: 'Delete', icon: 'trash', danger: true, run: async () => {
        await del('threads/' + t.id);
        load();
        toast('Chat deleted', { action: 'Undo', ms: 9000, run: () => post(`threads/${t.id}/restore`).then(load) });
      } },
    ], x, y);
  }
  q.oninput = draw;
  list.replaceChildren(loading());
  await load();
  const un = listen((ev) => (ev.kind === 'thread' || ev.kind === 'message' || ev.kind === 'resync') && !ev.project && load());
  return un;
}

export async function newChat() {
  const pick = agentModelPicker({ agentId: S.settings.default_lead || 'claude', model: S.agents[S.settings.default_lead || 'claude']?.config?.model ?? '' });
  const text = textarea('', { rows: 4, placeholder: 'First message (you can also send it in the chat)' });
  const folder = input('', { placeholder: 'Optional: a folder to work in. Empty: a fresh folder of its own.' });
  let made = null;
  const ok = await dialog({
    title: 'New chat',
    body: h('div', { class: 'dlg-fields' }, field('Agent', pick.el), field('Message', text),
      field('Folder', h('div', { class: 'row' }, folder, h('button', { class: 'btn', type: 'button', onclick: async () => {
        const f = await pickFolder({ start: folder.value });
        if (f) folder.value = f;
      } }, 'Browse')))),
    ok: 'Start chat',
    check: async () => {
      const { agent: a, model: m } = pick.get();
      try {
        made = await post('threads', { agent: a, model: m, text: text.value.trim(), folder: folder.value.trim(), title: text.value.trim().split('\n')[0].slice(0, 60) });
      } catch (e) {
        return e.message;
      }
    },
  });
  if (ok && made) openThread(made.id);
  return made;
}

export { agentName, go };
