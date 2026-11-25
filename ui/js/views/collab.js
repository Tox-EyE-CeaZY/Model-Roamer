// Collaboration management, both sides (see docs/collaboration.md):
// - Owner: `projectPeoplePanel(el, pid)` mounts inside a project's own Settings tab (and inline in Settings › Collaborators).
//   Settings › Collaborators is an overview of all own projects with direct links into each project's People area.
// - Guest: Settings › Join a project is for pasting an invite link and listing/leaving joined projects.
//   Joined projects open on the real project page (#/p/<link_id>), never inside Settings.

import { h, ago } from '../util.js';
import { get, post, patch, del, listen } from '../api.js';
import { S } from '../store.js';
import { go, leaveJoinedProject } from '../app.js';
import { dialog, toast } from '../ui.js';
import { ic, field, input, select, seg, toggle, errorBox, empty, kicker, copyable } from '../common.js';
import { chatsPanel } from './collab_chat.js';
import { presenceDot } from '../collab-presence.js';
import { ROLE_LABEL } from '../collab_perms.js';

const enc = encodeURIComponent;

// ------------------------------------------------------------ owner: reusable per-project People & Rules panel
/**
 * Mount Invite + People + Rules + Project chats management scoped to a single owner project `pid`.
 * Used both inside Project › Settings and inside Settings › Collaborators.
 */
export async function projectPeoplePanel(el, pid) {
  const inviteBox = h('section', { class: 'panel cut' });
  const peopleBox = h('section', { class: 'panel cut' });
  const rulesBox = h('section', { class: 'panel cut' });
  const chatsBox = h('section', { class: 'panel cut' });
  el.replaceChildren(inviteBox, peopleBox, rulesBox, chatsBox);

  let net = null;
  try {
    net = await get('network', { quiet: true });
  } catch { /* invite still works with default address */ }

  let data = null;
  let dead = false;
  const memberDots = new Map();

  async function refreshMemberDots() {
    const view = await get(`collab/projects/${enc(pid)}/presence`, { quiet: true }).catch(() => null);
    if (!view || dead) return;
    const online = new Set((view.people || []).filter((p) => p.online).map((p) => p.user_id));
    for (const [user, dot] of memberDots) dot.replaceChildren(presenceDot(online.has(user)));
  }

  async function draw() {
    try {
      data = await get('collab/projects/' + enc(pid));
    } catch (e) {
      if (!dead) peopleBox.replaceChildren(errorBox(e, draw));
      return;
    }
    if (dead) return;
    drawInvite(null);
    drawPeople();
    drawRules();
    chatsPanel(chatsBox, pid);
  }

  function drawInvite(made) {
    const listening = data?.listening || net?.listening;
    if (made) {
      const days = Math.max(1, Math.round((made.expires_at - Date.now()) / 86400000));
      inviteBox.replaceChildren(
        kicker('Invite link ready', h('span', { class: 'tag' }, ROLE_LABEL[made.role] || made.role)),
        h('div', { class: 'pair collab-pair' },
          h('div', { class: 'pair-qr', html: made.qr || '' }),
          h('div', { class: 'collab-made' },
            h('p', {}, h('b', {}, made.name), ' opens Model Roamer, chooses ', h('b', {}, '+ › Join a project'), ' (or ', h('b', {}, 'Settings › Join a project'), ') and pastes this link:'),
            copyable(made.link),
            h('p', { class: 'faint small' }, ic('lock', 12), ` Works once, for this one person, for ${days} ${days === 1 ? 'day' : 'days'}. Send it privately. You can withdraw it below until it’s used.`),
            h('button', { class: 'btn sm', onclick: () => drawInvite(null) }, 'Invite someone else'))),
      );
      return;
    }
    const name = input('', { placeholder: 'Their name, e.g. Sam' });
    let role = 'collaborator', hours = 168;
    const urls = net?.urls || [];
    const addr = urls.length ? select(urls.map((u) => [u.url, `${u.label} — ${u.url}`]), urls[0].url) : null;
    const go1 = h('button', {
      class: 'btn primary',
      type: 'button',
      onclick: async () => {
        if (!name.value.trim()) return name.focus();
        go1.disabled = true;
        try {
          const res = await post(`collab/projects/${enc(pid)}/invites`, { name: name.value.trim(), role, expires_hours: hours, base_url: addr?.value || '' });
          drawInvite({ ...res, name: name.value.trim(), role });
          draw2();
        } catch (e) {
          toast(e.message, { err: true });
        } finally {
          go1.disabled = false;
        }
      },
    }, ic('link', 14), 'Make invite link');
    inviteBox.replaceChildren(
      kicker('Invite a person'),
      !listening ? h('div', { class: 'collab-warn' }, ic('warn', 14), h('span', {}, 'Sharing is off, so nobody can reach this machine yet. Turn it on in ', h('a', { href: '#/settings/remote' }, 'Remote access'), ' first.')) : null,
      h('div', { class: 'row2' }, field('Name', name), field('Role', seg([['collaborator', 'Collaborator'], ['operator', 'Operator'], ['viewer', 'Viewer']], role, (v) => (role = v)))),
      h('p', { class: 'faint small' }, 'Operators may also take threads over from you or others without asking each time. Collaborators can ask to become one later; you approve once.'),
      field('Link works for', seg([[24, '1 day'], [168, '7 days'], [720, '30 days']], hours, (v) => (hours = +v))),
      addr ? field('Address they use', addr, 'Pick one they can reach: Tailscale for anywhere (private and encrypted), your domain with HTTPS, or the local network if they’re in the same place.') : null,
      go1,
    );
  }

  async function draw2() {
    try {
      data = await get('collab/projects/' + enc(pid));
    } catch { return; }
    if (!dead) drawPeople();
  }

  function drawPeople() {
    const { members, invites } = data;
    memberDots.clear();
    peopleBox.replaceChildren(
      kicker('People in this project', h('b', { class: 'count' }, members.length)),
      members.length ? h('div', { class: 'dev-list' }, members.map(memberRow)) : h('p', { class: 'faint small' }, 'Nobody has joined yet. Only you can see and change this project.'),
      invites.length ? h('div', { class: 'collab-sub' }, kicker('Waiting to be used'), h('div', { class: 'dev-list' }, invites.map((i) => h('div', { class: 'dev' }, ic('link', 15),
        h('div', {}, h('b', {}, i.name, ' ', h('span', { class: 'tag' }, ROLE_LABEL[i.role] || i.role)), h('small', { class: 'faint' }, `made ${ago(i.created_at)} · expires ${new Date(i.expires_at).toLocaleDateString()}`)),
        h('button', { class: 'btn xs', type: 'button', onclick: async () => {
          await del('collab/invites/' + i.id).catch((e) => toast(e.message, { err: true }));
          draw2();
        } }, 'Withdraw'))))) : null,
    );
    refreshMemberDots();
  }

  function memberRow(m) {
    const dot = h('span', { class: 'collab-member-dot' });
    memberDots.set(m.user_id, dot);
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
      h('button', { class: 'btn xs', type: 'button', onclick: async () => {
        await patch('collab/members/' + m.id, { perms: {} }).catch((e) => toast(e.message, { err: true }));
        draw2();
      } }, 'Back to the role’s rules'));
    const ask = m.requested_role ? h('div', { class: 'collab-ask cut' }, ic('flag', 14),
      h('span', {}, h('b', {}, m.name), ` asks to become ${ROLE_LABEL[m.requested_role] === 'Operator' ? 'an operator' : 'a ' + m.requested_role}: they could take threads over from you or others without asking each time.`),
      h('button', { class: 'btn xs primary', type: 'button', onclick: () => answer(m, true) }, 'Approve'),
      h('button', { class: 'btn xs', type: 'button', onclick: () => answer(m, false) }, 'Decline')) : null;
    return h('div', { class: 'collab-member' }, ask,
      h('div', { class: 'dev' }, dot,
        h('div', {}, h('b', {}, m.name), h('small', { class: 'faint' }, `joined ${ago(m.created_at)} · last seen ${m.last_seen ? ago(m.last_seen) : 'never'}${m.last_ip ? ' from ' + m.last_ip : ''}`)),
        select(data.roles.map((x) => [x, ROLE_LABEL[x]]), m.role, async (v) => {
          await patch('collab/members/' + m.id, { role: v }).catch((e) => toast(e.message, { err: true }));
          draw2();
        }, { class: 'input sel sm' }),
        h('button', { class: 'btn xs danger', type: 'button', onclick: async () => {
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
    rulesBox.replaceChildren(
      kicker('Rules', h('small', { class: 'faint' }, 'what each role may do in this project')),
      h('div', { class: 'collab-grid', style: { '--cols': roles.length } },
        h('div', { class: 'collab-perm head' }, h('span'), roles.map((r) => h('b', { class: 'mono' }, ROLE_LABEL[r]))),
        data.perms.map((p) => h('div', { class: 'collab-perm' }, h('span', {}, p.label),
          roles.map((role) => toggle(!!data.rules[role][p.id], async (v) => {
            try {
              data.rules = await patch(`collab/projects/${enc(pid)}/rules`, { [role]: { [p.id]: v } });
              draw2();
              chatsPanel(chatsBox, pid);
            } catch (e) {
              toast(e.message, { err: true });
            }
          }, { title: `${ROLE_LABEL[role]}: ${p.id}` }))))),
      h('p', { class: 'faint small' }, ic('shield', 12), ' Always only you: inviting, these rules, project settings and repositories, deleting, agents, terminals and this machine’s settings. Nobody else ever sees your logins, tokens or local folders.'),
    );
  }

  await draw();
  const un = listen((ev) => {
    if (ev.project !== pid) return;
    if (ev.kind === 'collab') draw2();
    if (ev.kind === 'presence') refreshMemberDots();
  });
  return () => {
    dead = true;
    un();
  };
}

// ------------------------------------------------------------ owner: Settings › Collaborators (overview + links to each project)
export async function collaborators(el, r) {
  let ov;
  try {
    ov = await get('collab');
  } catch (e) {
    return el.replaceChildren(errorBox(e, () => collaborators(el, r)));
  }
  const ownProjects = (ov.projects || []).filter((p) => !String(p.id).startsWith('lnk_'));
  if (!ownProjects.length) {
    return el.replaceChildren(h('section', { class: 'panel cut' }, empty('people', 'No own projects yet', 'Make a project first; then invite people to it from its Settings tab or from here.')));
  }
  let pid = ownProjects.some((p) => p.id === r.q.project) ? r.q.project : ownProjects[0].id;
  const overviewBox = h('section', { class: 'panel cut' });
  const detailHost = h('div', { class: 'collab-detail-host' });
  el.replaceChildren(overviewBox, detailHost);

  let stopDetail = null;
  async function mountSelected() {
    stopDetail?.();
    stopDetail = null;
    drawOverview();
    stopDetail = await projectPeoplePanel(detailHost, pid);
  }

  function drawOverview() {
    overviewBox.replaceChildren(
      kicker('Collaborators across your projects', h('small', { class: 'faint' }, 'manage per project in Project › Settings › People')),
      h('p', { class: 'faint small' }, 'You host your projects: threads, memory and artifacts stay on this machine. Each collaborator joins with a one-time link and runs their own agents on their own accounts.'),
      h('div', { class: 'dev-list' }, ownProjects.map((p) => h('div', { class: 'dev collab-ov-row' + (p.id === pid ? ' on' : '') },
        ic('board', 15),
        h('div', {},
          h('b', {}, p.name),
          h('small', { class: 'faint' }, `${p.members || 0} ${p.members === 1 ? 'person' : 'people'}${p.invites ? ` · ${p.invites} pending ${p.invites === 1 ? 'invite' : 'invites'}` : ''}`)),
        h('button', {
          class: 'btn xs' + (p.id === pid ? ' on' : ''),
          type: 'button',
          onclick: () => {
            pid = p.id;
            history.replaceState(history.state, '', '#/settings/collab?project=' + enc(p.id));
            mountSelected();
          },
        }, p.id === pid ? 'Managing below' : 'Manage here'),
        h('a', { class: 'btn xs primary', href: `#/p/${enc(p.id)}/settings?section=people` }, 'Open in project →')))),
    );
  }

  await mountSelected();
  const un = listen(async (ev) => {
    if (ev.kind !== 'collab') return;
    try {
      const fresh = await get('collab', { quiet: true });
      const next = (fresh.projects || []).filter((p) => !String(p.id).startsWith('lnk_'));
      if (next.length) {
        ownProjects.splice(0, ownProjects.length, ...next);
        drawOverview();
      }
    } catch { /* fine */ }
  });
  return () => {
    un();
    stopDetail?.();
  };
}

// ------------------------------------------------------------ guest: Settings › Join a project
export async function joinProject(el, r) {
  // Legacy #/settings/join?link=<id> links redirect straight to the real project page.
  if (r.q.link) {
    go('#/p/' + enc(r.q.link) + (r.q.inbox ? '?inbox=1' : ''));
    return;
  }
  const joinBox = h('section', { class: 'panel cut' });
  const listBox = h('section', { class: 'panel cut' });
  el.replaceChildren(joinBox, listBox);

  const link = input(r.q.invite ? location.href : '', { placeholder: 'https://their-machine.tailnet.ts.net:4630/#/join?invite=…' });
  const name = input(S.server?.name || '', { placeholder: 'Your name, as the others will see it' });
  const err = h('div', { class: 'dlg-err', hidden: true });
  const btn = h('button', { class: 'btn primary', type: 'submit' }, ic('login', 14), 'Join project');
  joinBox.replaceChildren(
    kicker('Join a project'),
    h('p', { class: 'faint small' }, 'Paste an invite link someone sent you. Once joined, the project appears in your sidebar Projects list like any other project. Your agents run here on your own accounts; logins and other projects are never shared.'),
    h('form', {
      class: 'collab-join',
      onsubmit: async (e) => {
        e.preventDefault();
        err.hidden = true;
        btn.disabled = true;
        try {
          const l = await post('collab/links', { link: link.value.trim(), name: name.value.trim() }, { timeout: 40000 });
          await post(`collab/links/${enc(l.id)}/view/ensure`, {}, { timeout: 25000 }).catch(() => {});
          toast(`Joined ${l.project_name} on ${l.host_name}`);
          go('#/p/' + enc(l.id));
        } catch (x) {
          err.textContent = x.message;
          err.hidden = false;
        } finally {
          btn.disabled = false;
        }
      },
    }, field('Invite link', link), field('Your name', name), err, btn),
  );

  async function drawList() {
    let list;
    try {
      list = await get('collab/links');
    } catch (e) {
      return listBox.replaceChildren(errorBox(e, drawList));
    }
    listBox.replaceChildren(
      kicker('Projects you joined', h('b', { class: 'count' }, list.length)),
      list.length ? h('div', { class: 'conn-list' }, list.map((l) => h('div', { class: 'conn cut' }, ic('people', 16),
        h('div', {},
          h('b', {}, l.project_name),
          h('small', { class: 'faint' }, `${l.host_name} · you are ${ROLE_LABEL[l.role] || l.role}${l.last_sync ? ' · synced ' + ago(l.last_sync) : ''}`),
          l.insecure ? h('small', { class: 'collab-bad' }, 'Plain http over the internet: ask the owner for a Tailscale or https address.') : null,
          l.error ? h('small', { class: 'collab-bad' }, l.error) : null),
        linkStatus(l),
        h('a', { class: 'btn sm primary', href: '#/p/' + enc(l.id) }, 'Open'),
        h('button', {
          class: 'btn sm',
          type: 'button',
          onclick: async () => {
            if (await leaveJoinedProject(l)) drawList();
          },
        }, 'Leave')))) : h('p', { class: 'faint small' }, 'None yet. Paste an invite link above, or choose + › Join a project in the sidebar.'),
    );
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
