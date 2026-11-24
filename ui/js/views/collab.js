// Collaboration, both sides (see docs/collaboration.md):
// - Settings › Collaborators (the owner): invite people to a project with a one-person link, set what each role may
//   do, change one person's permissions, remove people.
// - Settings › Join a project (a guest): paste an invite link, see the projects joined, and open one: the host's
//   threads, memory, artifacts and people, kept live by one long poll through this app (the host token never
//   reaches this window).

import { h, ago } from '../util.js';
import { get, post, patch, del, listen } from '../api.js';
import { S } from '../store.js';
import { go } from '../app.js';
import { openExternal } from '../native.js';
import { dialog, toast } from '../ui.js';
import { md } from '../md.js';
import { ic, field, input, textarea, select, seg, toggle, loading, errorBox, empty, kicker, copyable, statusChip } from '../common.js';
import { mountChat, chatsPanel, ensureLocal, threadTag } from './collab_chat.js';

const ROLE_LABEL = { collaborator: 'Collaborator', operator: 'Operator', viewer: 'Viewer', owner: 'Owner' };

// ------------------------------------------------------------ owner: Settings › Collaborators
export async function collaborators(el, r) {
  let ov;
  try {
    ov = await get('collab');
  } catch (e) {
    return el.replaceChildren(errorBox(e, () => collaborators(el, r)));
  }
  if (!ov.projects.length) {
    return el.replaceChildren(h('section', { class: 'panel cut' }, empty('people', 'No projects yet', 'Make a project first; then invite people to it from here.')));
  }
  let pid = ov.projects.some((p) => p.id === r.q.project) ? r.q.project : ov.projects[0].id;
  const top = h('section', { class: 'panel cut' });
  const inviteBox = h('section', { class: 'panel cut' });
  const peopleBox = h('section', { class: 'panel cut' });
  const rulesBox = h('section', { class: 'panel cut' });
  const chatsBox = h('section', { class: 'panel cut' });
  el.replaceChildren(top, inviteBox, peopleBox, rulesBox, chatsBox);

  let net = null;
  try {
    net = await get('network');
  } catch { /* the invite still works with the default address */ }

  top.replaceChildren(kicker('Collaborators', h('small', { class: 'faint' }, 'people on their own Model Roamer, working in your project')),
    h('p', { class: 'faint small' }, 'You host the project: its threads, memory and artifacts stay on this machine. Each person joins with their own one-time link and runs their own agents on their own accounts; logins are never shared. You decide what they may do, and can remove anyone at once.'),
    field('Project', select(ov.projects.map((p) => [p.id, `${p.name}${p.members ? ` · ${p.members} ${p.members === 1 ? 'person' : 'people'}` : ''}`]), pid, (v) => {
      pid = v;
      history.replaceState(history.state, '', '#/settings/collab?project=' + encodeURIComponent(v));
      draw();
    })));

  let data = null;
  async function draw() {
    try {
      data = await get('collab/projects/' + pid);
    } catch (e) {
      peopleBox.replaceChildren(errorBox(e, draw));
      return;
    }
    drawInvite(null);
    drawPeople();
    drawRules();
    chatsPanel(chatsBox, pid);
  }

  function drawInvite(made) {
    const listening = data?.listening || net?.listening;
    if (made) {
      const days = Math.max(1, Math.round((made.expires_at - Date.now()) / 86400000));
      inviteBox.replaceChildren(kicker('Invite link ready', h('span', { class: 'tag' }, made.role)),
        h('div', { class: 'pair collab-pair' }, h('div', { class: 'pair-qr', html: made.qr || '' }),
          h('div', { class: 'collab-made' },
            h('p', {}, h('b', {}, made.name), ' opens Model Roamer, goes to ', h('b', {}, 'Settings › Join a project'), ' and pastes this link:'),
            copyable(made.link),
            h('p', { class: 'faint small' }, ic('lock', 12), ` Works once, for this one person, for ${days} ${days === 1 ? 'day' : 'days'}. Send it privately. You can withdraw it below until it’s used.`),
            h('button', { class: 'btn sm', onclick: () => drawInvite(null) }, 'Invite someone else'))));
      return;
    }
    const name = input('', { placeholder: 'Their name, e.g. Sam' });
    let role = 'collaborator', hours = 168;
    const urls = net?.urls || [];
    const addr = urls.length ? select(urls.map((u) => [u.url, `${u.label} — ${u.url}`]), urls[0].url) : null;
    const go1 = h('button', { class: 'btn primary', onclick: async () => {
      if (!name.value.trim()) return name.focus();
      go1.disabled = true;
      try {
        const res = await post(`collab/projects/${pid}/invites`, { name: name.value.trim(), role, expires_hours: hours, base_url: addr?.value || '' });
        drawInvite({ ...res, name: name.value.trim(), role });
        draw2();
      } catch (e) {
        toast(e.message, { err: true });
      } finally {
        go1.disabled = false;
      }
    } }, ic('link', 14), 'Make invite link');
    inviteBox.replaceChildren(kicker('Invite a person'),
      !listening ? h('div', { class: 'collab-warn' }, ic('warn', 14), h('span', {}, 'Sharing is off, so nobody can reach this machine yet. Turn it on in ', h('a', { href: '#/settings/remote' }, 'Remote access'), ' first.')) : null,
      h('div', { class: 'row2' }, field('Name', name), field('Role', seg([['collaborator', 'Collaborator'], ['operator', 'Operator'], ['viewer', 'Viewer']], role, (v) => (role = v)))),
      h('p', { class: 'faint small' }, 'Operators may also take threads over from you or others without asking each time. Collaborators can ask to become one later; you approve once.'),
      field('Link works for', seg([[24, '1 day'], [168, '7 days'], [720, '30 days']], hours, (v) => (hours = +v))),
      addr ? field('Address they use', addr, 'Pick one they can reach: Tailscale for anywhere (private and encrypted), your domain with HTTPS, or the local network if they’re in the same place.') : null,
      go1);
  }

  // after making an invite, only the lists need redrawing (the link stays on screen)
  async function draw2() {
    try {
      data = await get('collab/projects/' + pid);
    } catch { return; }
    drawPeople();
  }

  function drawPeople() {
    const { members, invites } = data;
    peopleBox.replaceChildren(kicker('People in this project', h('b', { class: 'count' }, members.length)),
      members.length ? h('div', { class: 'dev-list' }, members.map(memberRow)) : h('p', { class: 'faint small' }, 'Nobody has joined yet. Only you can see and change this project.'),
      invites.length ? h('div', { class: 'collab-sub' }, kicker('Waiting to be used'), h('div', { class: 'dev-list' }, invites.map((i) => h('div', { class: 'dev' }, ic('link', 15),
        h('div', {}, h('b', {}, i.name, ' ', h('span', { class: 'tag' }, i.role)), h('small', { class: 'faint' }, `made ${ago(i.created_at)} · expires ${new Date(i.expires_at).toLocaleDateString()}`)),
        h('button', { class: 'btn xs', onclick: async () => {
          await del('collab/invites/' + i.id).catch((e) => toast(e.message, { err: true }));
          draw2();
        } }, 'Withdraw'))))) : null);
  }

  function memberRow(m) {
    const perms = h('details', { class: 'collab-perms' }, h('summary', {}, 'Permissions for ' + m.name),
      h('div', { class: 'collab-grid one', style: { '--cols': 1 } }, data.perms.map((p) => {
        const overridden = m.perms && typeof m.perms === 'object' && p.id in m.perms;
        return h('div', { class: 'collab-perm' }, h('span', {}, p.label, overridden ? h('em', { class: 'faint' }, ' (just for them)') : null),
          toggle(!!m.effective[p.id], async (v) => {
            try {
              await patch('collab/members/' + m.id, { perms: { ...(typeof m.perms === 'object' ? m.perms : {}), [p.id]: v } });
              draw2();
            } catch (e) {
              toast(e.message, { err: true });
            }
          }, { title: p.id }));
      })),
      h('button', { class: 'btn xs', onclick: async () => {
        await patch('collab/members/' + m.id, { perms: {} }).catch((e) => toast(e.message, { err: true }));
        draw2();
      } }, 'Back to the role’s rules'));
    const ask = m.requested_role ? h('div', { class: 'collab-ask cut' }, ic('flag', 14),
      h('span', {}, h('b', {}, m.name), ` asks to become ${ROLE_LABEL[m.requested_role] === 'Operator' ? 'an operator' : 'a ' + m.requested_role}: they could take threads over from you or others without asking each time.`),
      h('button', { class: 'btn xs primary', onclick: () => answer(m, true) }, 'Approve'),
      h('button', { class: 'btn xs', onclick: () => answer(m, false) }, 'Decline')) : null;
    return h('div', { class: 'collab-member' }, ask,
      h('div', { class: 'dev' }, ic('people', 15),
        h('div', {}, h('b', {}, m.name), h('small', { class: 'faint' }, `joined ${ago(m.created_at)} · last seen ${m.last_seen ? ago(m.last_seen) : 'never'}${m.last_ip ? ' from ' + m.last_ip : ''}`)),
        select(data.roles.map((x) => [x, ROLE_LABEL[x]]), m.role, async (v) => {
          await patch('collab/members/' + m.id, { role: v }).catch((e) => toast(e.message, { err: true }));
          draw2();
        }, { class: 'input sel sm' }),
        h('button', { class: 'btn xs danger', onclick: async () => {
          if (!(await dialog({ title: `Remove ${m.name}?`, text: 'Their app is shut out at once: the next thing it asks this machine is refused. What they already wrote here stays. To let them back in, send a new invite.', ok: 'Remove', danger: true }))) return;
          await del('collab/members/' + m.id).catch((e) => toast(e.message, { err: true }));
          draw2();
        } }, 'Remove')),
      perms);
  }

  async function answer(m, yes) {
    try {
      await patch('collab/members/' + m.id, { approve_request: yes });
      toast(yes ? `${m.name} is an operator now` : 'Declined');
      draw2();
    } catch (e) {
      toast(e.message, { err: true });
    }
  }

  function drawRules() {
    const roles = data.roles;
    rulesBox.replaceChildren(kicker('Rules', h('small', { class: 'faint' }, 'what each role may do in this project')),
      h('div', { class: 'collab-grid', style: { '--cols': roles.length } },
        h('div', { class: 'collab-perm head' }, h('span'), roles.map((r) => h('b', { class: 'mono' }, ROLE_LABEL[r]))),
        data.perms.map((p) => h('div', { class: 'collab-perm' }, h('span', {}, p.label),
          roles.map((role) => toggle(!!data.rules[role][p.id], async (v) => {
            try {
              data.rules = await patch(`collab/projects/${pid}/rules`, { [role]: { [p.id]: v } });
              draw2();
              chatsPanel(chatsBox, pid);
            } catch (e) {
              toast(e.message, { err: true });
            }
          }, { title: `${ROLE_LABEL[role]}: ${p.id}` }))))),
      h('p', { class: 'faint small' }, ic('shield', 12), ' Always only you: inviting, these rules, project settings and repositories, deleting, agents, terminals and this machine’s settings. Nobody else ever sees your logins, tokens or local folders.'));
  }

  await draw();
  const un = listen((ev) => ev.kind === 'collab' && ev.project === pid && draw2());
  return () => un();
}

// ------------------------------------------------------------ guest: Settings › Join a project
export async function joinProject(el, r) {
  if (r.q.link) return joined(el, r.q.link);
  const joinBox = h('section', { class: 'panel cut' });
  const listBox = h('section', { class: 'panel cut' });
  el.replaceChildren(joinBox, listBox);

  const link = input(r.q.invite ? location.href : '', { placeholder: 'https://their-machine.tailnet.ts.net:4630/#/join?invite=…' });
  const name = input(S.server?.name || '', { placeholder: 'Your name, as the others will see it' });
  const err = h('div', { class: 'dlg-err', hidden: true });
  const btn = h('button', { class: 'btn primary', type: 'submit' }, ic('login', 14), 'Join');
  joinBox.replaceChildren(kicker('Join a project'),
    h('p', { class: 'faint small' }, 'Someone invited you to their project: paste the link they sent. Their project stays on their machine; your agents run here, on your own accounts, and nothing of yours (logins, keys, other projects) is shared.'),
    h('form', { class: 'collab-join', onsubmit: async (e) => {
      e.preventDefault();
      err.hidden = true;
      btn.disabled = true;
      try {
        const l = await post('collab/links', { link: link.value.trim(), name: name.value.trim() }, { timeout: 40000 });
        toast(`Joined ${l.project_name} on ${l.host_name}`);
        go('#/settings/join?link=' + encodeURIComponent(l.id));
      } catch (x) {
        err.textContent = x.message;
        err.hidden = false;
      } finally {
        btn.disabled = false;
      }
    } }, field('Invite link', link), field('Your name', name), err, btn));

  async function drawList() {
    let list;
    try {
      list = await get('collab/links');
    } catch (e) {
      return listBox.replaceChildren(errorBox(e, drawList));
    }
    listBox.replaceChildren(kicker('Projects you joined', h('b', { class: 'count' }, list.length)),
      list.length ? h('div', { class: 'conn-list' }, list.map((l) => h('div', { class: 'conn cut' }, ic('people', 16),
        h('div', {}, h('b', {}, l.project_name), h('small', { class: 'faint' }, `${l.host_name} · you are ${ROLE_LABEL[l.role] || l.role}${l.last_sync ? ' · synced ' + ago(l.last_sync) : ''}`),
          l.insecure ? h('small', { class: 'collab-bad' }, 'Plain http over the internet: ask the owner for a Tailscale or https address.') : null,
          l.error ? h('small', { class: 'collab-bad' }, l.error) : null),
        linkStatus(l),
        h('button', { class: 'btn sm primary', disabled: l.status === 'revoked', onclick: () => go('#/settings/join?link=' + encodeURIComponent(l.id)) }, 'Open'),
        h('button', { class: 'btn sm', onclick: async () => {
          if (!(await dialog({ title: `Leave ${l.project_name}?`, text: `You stop seeing it, and ${l.host_name} is told. To come back you need a new invite.`, ok: 'Leave', danger: true }))) return;
          await del('collab/links/' + l.id).catch((e) => toast(e.message, { err: true }));
          drawList();
        } }, 'Leave')))) : h('p', { class: 'faint small' }, 'None yet.'));
  }
  await drawList();
  const un = listen((ev) => ev.kind === 'collab-links' && drawList());
  return () => un();
}

function linkStatus(l) {
  if (l.status === 'revoked') return h('span', { class: 'status hot' }, 'Removed');
  if (l.status === 'offline') return h('span', { class: 'status dim' }, 'Offline');
  return h('span', { class: 'status acc' }, 'Joined');
}

// ------------------------------------------------------------ guest: one joined project, live
async function joined(el, linkId) {
  const base = `collab/links/${encodeURIComponent(linkId)}/host/`;
  const links = await get('collab/links').catch(() => []);
  const link = links.find((l) => l.id === linkId);
  if (!link) return el.replaceChildren(h('section', { class: 'panel cut' }, empty('people', 'Not joined', 'This project isn’t in your list any more.', h('a', { class: 'btn', href: '#/settings/join' }, 'Back'))));
  const head = h('section', { class: 'panel cut collab-head' });
  const body = h('section', { class: 'panel cut' });
  el.replaceChildren(head, body);
  body.replaceChildren(loading('Reaching ' + link.host_name));

  let st;
  try {
    st = await get(base + 'project', { timeout: 30000 });
  } catch (e) {
    body.replaceChildren(errorBox(e, () => joined(el, linkId)));
    head.replaceChildren(h('a', { class: 'linkbtn', href: '#/settings/join' }, ic('left', 13), 'All joined projects'));
    return;
  }
  const threads = new Map(st.threads.map((t) => [t.id, t]));
  const memory = new Map();
  const artifacts = new Map();
  let people = st.people || [];
  let cursor = st.cursor;
  let tab = 'chat', openThread = null, live = 'ok', alive = true, stopChat = null;
  const perms = st.perms || {};
  const me = st.member;

  const liveTag = h('span');
  function drawHead() {
    liveTag.replaceChildren(live === 'ok' ? h('span', { class: 'status live' }, h('i', { class: 'pulse' }), 'Live') : live === 'revoked' ? h('span', { class: 'status hot' }, 'Removed') : h('span', { class: 'status dim' }, 'Offline, retrying'));
    head.replaceChildren(
      h('a', { class: 'linkbtn', href: '#/settings/join' }, ic('left', 13), 'All joined projects'),
      h('div', { class: 'collab-title' }, h('h2', {}, st.project?.name || link.project_name), liveTag),
      h('div', { class: 'collab-title' }, h('p', { class: 'faint small' }, `Hosted by ${st.host?.name || link.host_name} · you are ${ROLE_LABEL[me.role] || me.role} as ${me.name}`), askOperator()),
      seg([['chat', 'Your chat'], ['threads', 'Threads'], ['memory', 'Memory'], ['artifacts', 'Artifacts'], ['people', 'People']], tab, (v) => {
        tab = v;
        openThread = null;
        drawBody();
      }));
  }

  // an operator takes threads over without asking each time: asked for once, approved once by the owner
  function askOperator() {
    if (me.role === 'operator' || me.role === 'viewer') return null;
    if (me.requested_role) return h('span', { class: 'tag' }, 'Operator request sent');
    return h('button', { class: 'btn xs', title: 'Ask the owner once; then you can take threads over without asking each time', onclick: async () => {
      if (!(await dialog({ title: 'Ask to become an operator?', text: `${st.host?.name || link.host_name} decides once. As an operator you can take threads over from them or others without asking each time.`, ok: 'Ask' }))) return;
      try {
        await post(base + 'request-role', { role: 'operator' });
        me.requested_role = 'operator';
        toast('Asked. You’ll see your role change here once they answer.');
        drawHead();
      } catch (e) {
        toast(e.message, { err: true });
      }
    } }, ic('flag', 12), 'Ask to be an operator');
  }

  function drawBody() {
    if (stopChat) stopChat = (stopChat(), null);
    if (tab === 'chat') return mountChat(body, link).then((s) => (alive && tab === 'chat' ? (stopChat = s) : s()));
    if (tab === 'threads') return openThread ? drawThread(openThread) : drawThreads();
    if (tab === 'memory') return drawMemory();
    if (tab === 'artifacts') return drawArtifacts();
    return drawPeople();
  }

  function drawThreads() {
    // the person's own chat has its own tab; their own threads open here, on this machine
    const list = [...threads.values()].filter((t) => !t.deleted_at && !(t.kind === 'chat' && t.user_id === me.user_id));
    body.replaceChildren(
      kicker('Threads', perms.create_threads ? h('button', { class: 'btn xs', onclick: newThread }, ic('plus', 12), 'New thread') : null),
      list.length ? h('div', { class: 'dev-list' }, list.map((t) => h('button', { class: 'collab-thread', onclick: () => (t.user_id === me.user_id ? go('#/t/' + t.id) : ((openThread = t.id), drawBody())) },
        h('div', {}, h('b', {}, t.kind === 'lead' ? '★ ' : '', t.title), h('small', { class: 'faint' }, `${t.agent}${threadTag(t, me, personName)} · ${ago(t.updated_at)}`),
          t.last_body ? h('small', { class: 'collab-last' }, t.last_body) : null),
        statusChip(t)))) : h('p', { class: 'faint small' }, perms.view_threads ? 'No threads yet.' : 'The owner hasn’t shared threads with you.'));
  }

  const personName = (uid) => people.find((p) => p.user_id === uid)?.name || 'someone';

  async function newThread() {
    // it opens here, on this machine, and goes up to the host from there
    try {
      await ensureLocal(link);
      const { newThreadDialog } = await import('./project.js');
      return newThreadDialog(link.id);
    } catch (e) {
      toast(e.message, { err: true });
    }
  }

  const msgs = new Map(); // thread id -> Map(message id -> message)
  async function drawThread(id) {
    const t = threads.get(id);
    if (!t) return drawThreads();
    const box = h('div', { class: 'collab-msgs' }, loading());
    const own = t.user_id === me.user_id;
    const may = own || (t.kind === 'lead' ? perms.message_lead : perms.message_threads);
    const text = textarea('', { rows: 3, placeholder: may ? (own ? 'Write to your thread' : 'Write to the owner’s agents (uses their plan)') : 'The owner hasn’t allowed you to write here', disabled: !may });
    const send = h('button', { class: 'btn primary sm', disabled: !may, onclick: async () => {
      if (!text.value.trim()) return;
      send.disabled = true;
      try {
        const m = await post(base + `threads/${id}/messages`, { text: text.value });
        addMsgs([m]);
        text.value = '';
        paintMsgs();
      } catch (e) {
        toast(e.message, { err: true });
      } finally {
        send.disabled = false;
      }
    } }, ic('send', 13), 'Send');
    body.replaceChildren(
      h('button', { class: 'linkbtn', onclick: () => ((openThread = null), drawBody()) }, ic('left', 13), 'Threads'),
      h('div', { class: 'collab-title' }, h('h3', {}, t.title), statusChip(t)),
      t.instructions ? h('details', { class: 'adv' }, h('summary', {}, 'Instructions'), h('div', { class: 'md', html: md(t.instructions) })) : null,
      box, h('div', { class: 'collab-compose' }, text, send));
    try {
      const r = await get(base + `threads/${id}/messages?limit=120`);
      addMsgs(r.messages);
    } catch (e) {
      box.replaceChildren(errorBox(e));
      return;
    }
    paintMsgs();
  }
  function addMsgs(list) {
    for (const m of list) {
      if (!msgs.has(m.thread_id)) msgs.set(m.thread_id, new Map());
      msgs.get(m.thread_id).set(m.id, m);
    }
  }
  function paintMsgs() {
    const box = body.querySelector('.collab-msgs');
    if (!box || !openThread) return;
    const list = [...(msgs.get(openThread)?.values() || [])].sort((a, b) => a.created_at - b.created_at);
    box.replaceChildren(...(list.length ? list.map((m) => h('div', { class: 'collab-msg ' + m.role },
      h('div', { class: 'collab-msg-h' }, h('b', {}, m.author || m.role), h('small', { class: 'faint' }, ago(m.created_at))),
      h('div', { class: 'md', html: md(m.body) }))) : [h('p', { class: 'faint small' }, 'No messages yet.')]));
    box.scrollTop = box.scrollHeight;
  }

  async function drawMemory() {
    body.replaceChildren(kicker('Project memory', perms.edit_memory ? h('button', { class: 'btn xs', onclick: () => editMemory() }, ic('plus', 12), 'New entry') : null), loading());
    try {
      const list = await get(base + 'memory');
      memory.clear();
      list.forEach((m) => memory.set(m.id, m));
    } catch (e) {
      return body.replaceChildren(errorBox(e, drawMemory));
    }
    paintMemory();
  }
  function paintMemory() {
    const list = [...memory.values()].filter((m) => !m.deleted_at);
    body.replaceChildren(kicker('Project memory', perms.edit_memory ? h('button', { class: 'btn xs', onclick: () => editMemory() }, ic('plus', 12), 'New entry') : null),
      list.length ? h('div', { class: 'dev-list' }, list.map((m) => h('details', { class: 'collab-mem', ontoggle: async (e) => {
        const d = e.currentTarget;
        if (!d.open || d.dataset.loaded) return;
        d.dataset.loaded = '1';
        try {
          const full = await get(base + 'memory/' + encodeURIComponent(m.name));
          d.querySelector('.md').innerHTML = md(full.body);
        } catch (x) {
          d.querySelector('.md').textContent = x.message;
        }
      } }, h('summary', {}, h('b', { class: 'mono' }, m.name), m.project_id ? null : h('span', { class: 'tag' }, 'global'), h('small', { class: 'faint' }, ' ', m.description || '')),
        h('div', { class: 'md' }, '…'),
        perms.edit_memory && m.project_id ? h('button', { class: 'btn xs', onclick: () => editMemory(m) }, ic('edit', 12), 'Edit') : null))) : h('p', { class: 'faint small' }, 'Memory is empty.'));
  }
  async function editMemory(m) {
    const name = input(m?.name || '', { placeholder: 'short-kebab-name', disabled: !!m });
    const desc = input(m?.description || '', { placeholder: 'One line: what this entry is for' });
    const bodyT = textarea('', { rows: 10 });
    if (m) get(base + 'memory/' + encodeURIComponent(m.name)).then((f) => (bodyT.value = f.body)).catch(() => {});
    if (!(await dialog({ title: m ? 'Edit ' + m.name : 'New memory entry', wide: true, body: h('div', {}, field('Name', name), field('Description', desc), field('Body', bodyT)), ok: 'Save' }))) return;
    try {
      await post(base + 'memory', { name: name.value, description: desc.value, body: bodyT.value });
      toast('Saved to the project’s memory');
      drawMemory();
    } catch (e) {
      toast(e.message, { err: true });
    }
  }

  async function drawArtifacts() {
    body.replaceChildren(kicker('Artifacts'), loading());
    try {
      const list = await get(base + 'artifacts');
      artifacts.clear();
      list.forEach((a) => artifacts.set(a.id, a));
    } catch (e) {
      return body.replaceChildren(errorBox(e, drawArtifacts));
    }
    paintArtifacts();
  }
  function paintArtifacts() {
    const list = [...artifacts.values()].filter((a) => !a.deleted_at).sort((a, b) => b.updated_at - a.updated_at);
    body.replaceChildren(kicker('Artifacts', h('b', { class: 'count' }, list.length)),
      list.length ? h('div', { class: 'dev-list' }, list.map((a) => h('div', { class: 'dev' }, ic('artifact', 15),
        h('div', {}, h('b', {}, a.title), h('small', { class: 'faint' }, `${a.kind} · v${a.version} · ${ago(a.updated_at)}`)),
        h('button', { class: 'btn xs', onclick: () => openExternal(link.host_url + a.url) }, ic('external', 12), 'Open')))) : h('p', { class: 'faint small' }, 'No artifacts yet.'));
  }

  function drawPeople() {
    body.replaceChildren(kicker('People', h('b', { class: 'count' }, people.length)),
      h('div', { class: 'dev-list' }, people.map((p) => h('div', { class: 'dev' }, ic('people', 15),
        h('div', {}, h('b', {}, p.name, p.user_id === me.user_id ? h('span', { class: 'tag' }, 'you') : null), h('small', { class: 'faint' }, (ROLE_LABEL[p.role] || p.role) + (p.last_seen ? ' · seen ' + ago(p.last_seen) : ''))))))
      , h('p', { class: 'faint small' }, 'What each person is working on right now arrives with presence, in a later update.'));
  }

  // one long poll at a time: answers at once when something changed, otherwise waits on the host for up to 20 s
  async function pump() {
    let delay = 2000;
    while (alive) {
      try {
        const c = await get(base + `changes?since=${cursor}&wait=20`, { timeout: 45000, quiet: true });
        cursor = c.cursor;
        if (c.people) people = c.people;
        c.threads.forEach((t) => threads.set(t.id, { ...threads.get(t.id), ...t }));
        addMsgs(c.messages);
        c.memory.forEach((m) => memory.set(m.id, m));
        c.artifacts.forEach((a) => artifacts.set(a.id, a));
        if (live !== 'ok') {
          live = 'ok';
          drawHead();
        }
        delay = 2000;
        if (!alive) return;
        if (tab === 'threads' && !openThread && (c.threads.length || c.messages.length)) drawThreads();
        else if (tab === 'threads' && openThread && c.messages.some((m) => m.thread_id === openThread)) paintMsgs();
        else if (tab === 'memory' && c.memory.length) paintMemory();
        else if (tab === 'artifacts' && c.artifacts.length) paintArtifacts();
        else if (tab === 'people') drawPeople();
        if (c.more) continue;
      } catch (e) {
        if (!alive) return;
        const revoked = e.status === 403 && /removed|withdrawn|isn’t valid|isn't valid/i.test(e.message);
        live = revoked ? 'revoked' : 'offline';
        drawHead();
        if (revoked) {
          body.replaceChildren(errorBox(e));
          return;
        }
        await new Promise((r) => setTimeout(r, delay));
        delay = Math.min(30000, delay * 2);
      }
    }
  }

  drawHead();
  drawBody();
  pump();
  return () => {
    alive = false;
    if (stopChat) stopChat();
  };
}
