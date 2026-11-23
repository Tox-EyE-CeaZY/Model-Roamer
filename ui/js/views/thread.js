// A thread: the conversation with one agent, its live work, its decisions, and what to do with its branch.
// The same component is the project chat on the board (compact) and a thread in its own window (pop-out).

import { h, ago, clock, dayLabel, dur, num, bytes } from '../util.js';
import { icon } from '../icons.js';
import { get, post, patch, del, listen, upload } from '../api.js';
import { S, watch, agentName, agentHue, project as projectOf, rememberChatPick } from '../store.js';
import { go, back, openThread } from '../app.js';
import { isPop, isDesktop, popOut, setTitle, openExternal, native } from '../native.js';
import { dialog, toast, showMenu } from '../ui.js';
import { md, wireMd } from '../md.js';
import { ic, stamp, statusChip, statusLabel, field, input, textarea, toggle, agentModelPicker, agentOptions, select, loading, errorBox, empty, timeAgo, kicker, copyable, pickFolder } from '../common.js';
import { runStep } from '../term.js';

export default async function threadView(el, r, ctx) {
  return mountThread(el, r.id, { full: true, ctx });
}

const TOOL_ICONS = { shell: 'terminal', 'edit files': 'edit', 'web search': 'globe' };

const COMPACT_AT = 90;
/** The thread's context fill: {used, window, pct, compactions, ...}, or null when its agent doesn't report it. */
export function ctxOf(t) {
  const c = t?.context;
  if (!c || typeof c !== 'object' || c.agent !== t.agent || !c.window) return null;
  return { ...c, pct: Math.min(999, Math.round((c.used / c.window) * 100)) };
}

export async function mountThread(el, id, { full, compact, ctx } = {}) {
  let t;
  try {
    t = await get('threads/' + id);
  } catch (e) {
    el.replaceChildren(errorBox(e, () => mountThread(el, id, { full, compact, ctx })));
    return;
  }
  const head = h('div', { class: 'th-head' });
  const decisionsEl = h('div', { class: 'th-decisions' });
  const log = h('div', { class: 'th-log' });
  const liveEl = h('div', { class: 'th-live', hidden: true });
  const scroller = h('div', { class: 'th-scroll' }, h('div', { class: 'th-inner' }, log, liveEl));
  const composer = makeComposer();
  const sidebar = full ? h('aside', { class: 'th-side' }) : null;
  el.classList.add('thread', compact ? 'compact' : 'full');
  el.replaceChildren(
    compact ? null : head,
    h('div', { class: 'th-body' }, h('div', { class: 'th-main' }, decisionsEl, scroller, composer.el), sidebar));

  let messages = [], turns = [], activity = {}, hasMore = false;
  // shown: how much of the streamed text is on screen; it catches up word by word, so every agent's reply reads as it
  // is written, whether the agent sends tokens (Claude, Antigravity) or whole paragraphs (Codex). hold: a finished reply
  // that is still being typed out, moved into the log when the typing reaches its end.
  // parts: the turn so far in the order it happened: text the agent wrote and its steps (thinking, tools, results).
  // Only the trailing text is typed out; text before a step is already on screen.
  const live = { turn: null, parts: [], started: 0, shown: 0, hold: null };
  const steps = stepView(() => t.steps || S.settings.steps || 'collapsed');
  const trailing = () => (live.parts.at(-1)?.k === 'text' ? live.parts.at(-1) : null);
  const liveTarget = () => (live.hold ? live.hold.body : trailing()?.text || '');
  const resetLive = (turn, started = Date.now()) => Object.assign(live, { turn, parts: [], started, shown: 0 });
  /** Text streamed in: added to the open text part, or a new part after a step. A block replaces what streamed. */
  function liveText(text, block) {
    const last = trailing();
    if (last?.open) {
      if (block) Object.assign(last, { text, open: false });
      else last.text += text;
      return;
    }
    if (block && last && last.text.trim() === text.trim()) return;
    live.parts.push({ k: 'text', text, open: !block });
    live.shown = 0;
  }
  function liveStep(a) {
    if (a.kind === 'text') return liveText(a.body || '', true);
    const last = trailing();
    if (last) last.open = false;
    live.parts.push({ k: 'act', a });
  }

  // ---------------------------------------------------------- header
  function drawHead() {
    const p = t.project;
    if (full) setTitle(`${t.title} · ${agentName(t.agent)}`);
    ctx?.setPopTitle?.(t.title);
    if (compact) return;
    const running = !!t.running;
    const isWorker = t.kind === 'worker' || t.kind === 'review';
    const canBranch = !!t.branch;
    const title = h('h1', { class: 'th-title', title: 'Rename', tabindex: '0', onclick: rename, onkeydown: (e) => e.key === 'Enter' && rename() }, t.title);
    head.replaceChildren(
      h('div', { class: 'th-crumbs mono' },
        !isPop && full ? h('button', { class: 'ibtn xs th-back', title: 'Back (Alt+Left)', onclick: () => back(p ? '#/p/' + p.id : '#/chats'), html: icon('back', 14) }) : null,
        p ? h('a', { href: '#/p/' + p.id, style: { '--ph': p.color } }, h('i', { class: 'sw' }), p.name) : h('a', { href: '#/chats' }, 'Chats'),
        h('span', {}, '/'),
        h('span', {}, t.kind === 'lead' ? 'project chat' : t.kind === 'review' ? 'review' : t.kind === 'chat' ? 'chat' : 'thread')),
      h('div', { class: 'th-title-row' }, title, stamp(t.agent, { model: t.model, lead: t.kind === 'lead' }), statusChip(t), buddyChip(), ctxChip()),
      t.status_note ? h('div', { class: 'th-note' }, ic(t.status === 'blocked' || t.status === 'failed' ? 'warn' : 'info', 14), h('span', {}, t.status_note)) : null,
      h('div', { class: 'th-acts' },
        running
          ? [h('button', { class: 'btn sm', onclick: () => act('pause'), title: 'Pause after stopping this turn' }, ic('pause', 13), 'Pause'), h('button', { class: 'btn sm danger', onclick: () => act('stop'), title: 'Stop (Ctrl+.)' }, ic('stop', 13), 'Stop')]
          : t.status === 'paused' || t.status === 'limited' || t.status === 'cancelled'
            ? h('button', { class: 'btn sm primary', onclick: () => act('resume') }, ic('play', 13), 'Resume')
            : t.status === 'failed'
              ? h('button', { class: 'btn sm primary', onclick: () => act('retry') }, ic('refresh', 13), 'Retry')
              : null,
        canBranch ? h('button', { class: 'btn sm', onclick: () => showDiff() }, ic('diff', 13), 'Changes') : null,
        canBranch && !t.merged ? h('button', { class: 'btn sm', onclick: () => doMerge() }, ic('merge', 13), 'Merge') : null,
        canBranch ? h('button', { class: 'btn sm', onclick: () => doPr() }, ic('pr', 13), 'Pull request') : null,
        isWorker || t.kind === 'chat' ? h('button', { class: 'btn sm', onclick: () => act(t.resolved ? 'reopen' : 'resolve') }, ic(t.resolved ? 'undo' : 'check', 13), t.resolved ? 'Reopen' : 'Resolve') : null,
        h('span', { class: 'grow' }),
        !isPop ? h('button', { class: 'ibtn', title: 'Pop out into its own window (Ctrl+Shift+O)', onclick: () => popOut('#/t/' + id, { key: 't:' + id }), html: icon('popout', 16) }) : null,
        h('button', { class: 'ibtn', title: 'More', onclick: (e) => moreMenu(e.currentTarget), html: icon('more', 16) })));
  }

  /** Buddy threads: which pair, whose turn it is, and how many turns it has used. */
  function buddyChip() {
    const b = t.buddy;
    if (!b || typeof b !== 'object') return null;
    const def = (S.settings.buddies || []).find((x) => x.id === b.id);
    const max = def?.max_turns || 4;
    return h('span', { class: 'ctx-chip buddy', title: `${b.name || 'Buddies'}: two agents taking turns. Turn ${b.turns || 0} of ${max}.` }, '⇄ ' + (b.name || 'Buddies'), h('small', {}, ` ${b.turns || 0}/${max}`));
  }

  function ctxChip() {
    const c = ctxOf(t);
    if (!c || c.pct < 60) return null;
    return h('span', { class: 'ctx-chip' + (c.pct >= 85 ? ' hot' : ''), title: `${num(c.used)} of ${num(c.window)} tokens in use. It compacts its conversation on its own around ${COMPACT_AT}%.` }, 'Context ' + c.pct + '%');
  }

  async function rename() {
    const i = input(t.title);
    if (!(await dialog({ title: 'Rename thread', body: field('Title', i), ok: 'Rename' }))) return;
    if (i.value.trim()) t = { ...t, ...(await patch('threads/' + id, { title: i.value.trim() })) };
    drawHead();
  }

  function moreMenu(anchor) {
    const r = anchor.getBoundingClientRect();
    const isWorker = t.kind === 'worker' || t.kind === 'review';
    showMenu([
      { label: 'Rename…', icon: 'edit', run: rename },
      { label: t.pinned ? 'Unpin' : 'Pin', icon: 'pin', run: async () => ((t = { ...t, ...(await patch('threads/' + id, { pinned: t.pinned ? 0 : 1 })) }), drawHead()) },
      { label: 'Hand to another agent…', icon: 'agents', run: reassign },
      isWorker || t.kind === 'chat' ? { label: 'Ask for a review…', icon: 'eye', run: review, disabled: t.kind === 'chat' } : null,
      { label: 'Start a fresh session', icon: 'refresh', title: 'The agent forgets its session; the next turn gets a recap of this thread instead', run: () => act('new_session').then(() => toast('The next turn starts a fresh session with a recap')) },
      { sep: true },
      t.workspace_path ? { label: 'Copy the folder path', icon: 'copy', run: () => navigator.clipboard?.writeText(t.workspace_path).then(() => toast('Copied')) } : null,
      t.workspace_path && S.me.local ? { label: 'Show the folder', icon: 'folder', run: () => act('reveal') } : null,
      t.branch ? { label: 'Copy the branch name', icon: 'branch', run: () => navigator.clipboard?.writeText(t.branch).then(() => toast('Copied')) } : null,
      { label: 'Turn logs…', icon: 'terminal', run: showTurns },
      { sep: true },
      t.kind !== 'lead' ? { label: 'Delete thread', icon: 'trash', danger: true, run: remove } : null,
    ], r.right, r.bottom + 4, { anchor });
  }

  async function act(action, extra = {}) {
    try {
      t = { ...t, ...(await post(`threads/${id}/action`, { action, ...extra })) };
      drawHead();
      drawSide();
    } catch (e) {
      toast(e.message, { err: true });
    }
  }

  async function remove() {
    if (!(await dialog({ title: 'Delete this thread?', text: 'The agent stops, and the thread leaves the board. Its branch stays in git.', ok: 'Delete', danger: true }))) return;
    try {
      await del('threads/' + id);
      toast('Thread deleted', { action: 'Undo', ms: 9000, run: () => post(`threads/${id}/restore`).then(() => openThread(id)) });
      if (isPop) window.close();
      else go(t.project_id ? '#/p/' + t.project_id : '#/chats');
    } catch (e) {
      toast(e.message, { err: true });
    }
  }

  async function reassign() {
    const pick = agentModelPicker({ agentId: t.agent === 'claude' ? 'codex' : 'claude', model: '' });
    const note = textarea('', { rows: 3, placeholder: 'Optional: why, or what the new agent should know' });
    if (!(await dialog({ title: 'Hand this thread to another agent', text: 'The new agent picks up with a recap of everything said so far, in the same workspace.', body: h('div', { class: 'dlg-fields' }, field('Agent', pick.el), field('Note', note)), ok: 'Hand over' }))) return;
    const { agent: a, model: m } = pick.get();
    act('reassign', { agent: a, model: m, note: note.value });
  }

  async function review() {
    const opts = agentOptions().filter(([k]) => k !== t.agent);
    const a = select(opts, opts[0]?.[0]);
    const focus = textarea('', { rows: 3, placeholder: 'What should the reviewer look hardest at? Optional.' });
    if (!(await dialog({ title: 'Ask another agent to review', text: 'A review thread reads this branch’s changes and reports back to the lead. A second pair of eyes from a different model catches different things.', body: h('div', { class: 'dlg-fields' }, field('Reviewer', a), field('Focus', focus)), ok: 'Ask for review' }))) return;
    try {
      const r = await post(`threads/${id}/action`, { action: 'review', agent: a.value, focus: focus.value });
      toast('Review started', { action: 'Open', run: () => openThread(r.id) });
    } catch (e) {
      toast(e.message, { err: true });
    }
  }

  // ---------------------------------------------------------- git: diff, merge, PR
  async function showDiff() {
    const body = h('div', { class: 'diffbox' }, loading('Reading the changes'));
    const p = dialog({ title: `Changes on ${t.branch}`, wide: true, cls: 'xwide', body, ok: t.merged ? null : 'Merge…', cancel: 'Close' });
    try {
      const r = await get(`threads/${id}/diff`);
      body.replaceChildren(r.diff ? renderDiff(r.diff) : h('p', { class: 'faint' }, r.note || 'No changes yet against ' + (r.base || 'the base branch') + '.'));
    } catch (e) {
      body.replaceChildren(errorBox(e));
    }
    if (await p) doMerge();
  }

  async function doMerge() {
    const msg = input(`${t.title} (${t.agent})`);
    let squash = false;
    const ok = await dialog({
      title: 'Merge into ' + (t.base_branch || 'the base branch'),
      text: 'The branch is merged in the project folder. If git finds a conflict nothing changes, and you can hand the conflict to an agent.',
      body: h('div', { class: 'dlg-fields' }, field('Commit message', msg), toggle(false, (v) => (squash = v), { label: 'Squash into one commit' })),
      ok: 'Merge',
    });
    if (!ok) return;
    try {
      const r = await post(`threads/${id}/merge`, { message: msg.value, squash });
      toast(`Merged (${String(r.sha).slice(0, 7)})`);
      t = await get('threads/' + id);
      drawHead();
    } catch (e) {
      const fix = await dialog({ title: 'The merge didn’t go through', text: e.message, ok: 'Ask the agent to fix it', cancel: 'Close' });
      if (fix) send(`Merging your branch into ${t.base_branch || 'the base branch'} failed:\n\n${e.message}\n\nBring your branch up to date with it (merge or rebase), resolve the conflicts, run the checks again and commit.`);
    }
  }

  async function doPr() {
    const title = input(t.title);
    const body = textarea(t.instructions ? `## What\n${t.instructions.slice(0, 1500)}\n` : '', { rows: 6 });
    let draft = false;
    const ok = await dialog({
      title: 'Open a pull request',
      text: `Pushes ${t.branch} to GitHub and opens a pull request into ${t.base_branch || 'the default branch'}.`,
      wide: true,
      body: h('div', { class: 'dlg-fields' }, field('Title', title), field('Description', body), toggle(false, (v) => (draft = v), { label: 'Open as a draft' })),
      ok: 'Open pull request',
    });
    if (!ok) return;
    try {
      const r = await post(`threads/${id}/pr`, { title: title.value, body: body.value, draft });
      toast(r.existing ? `Pull request #${r.number} already open` : `Opened pull request #${r.number}`, { action: 'View', ms: 8000, run: () => openExternal(r.url) });
    } catch (e) {
      toast(e.message, { err: true, ms: 8000 });
    }
  }

  async function showTurns() {
    const list = h('div', { class: 'turns' });
    const pane = h('pre', { class: 'logpane' }, 'Pick a turn to see its log.');
    for (const tu of turns)
      list.append(h('button', { class: 'turn-i', type: 'button', onclick: async () => {
        pane.textContent = 'Loading…';
        pane.textContent = (await get('logs?turn=' + tu.id).catch((e) => ({ text: e.message }))).text;
      } }, statusChip(tu.status === 'running' ? 'working' : tu.status), h('span', {}, agentName(tu.agent) + (tu.model ? ' · ' + tu.model : '')), h('small', { class: 'faint' }, ago(tu.started_at) + (tu.ended_at ? ' · ' + dur(tu.ended_at - tu.started_at) : ''))));
    dialog({ title: 'Turns', wide: true, cls: 'xwide', ok: null, cancel: 'Close', body: h('div', { class: 'turns-wrap' }, turns.length ? list : h('p', { class: 'faint' }, 'No turns yet.'), pane) });
  }

  // ---------------------------------------------------------- side panel
  function drawSide() {
    if (!sidebar) return;
    const u = t.usage || {};
    const deps = (t.depends_on || '').split(',').filter(Boolean);
    sidebar.replaceChildren(
      t.instructions ? h('section', {}, kicker('The task'), h('div', { class: 'md small' }, ...mdNodes(t.instructions))) : null,
      t.criteria ? h('section', {}, kicker('Done when'), h('div', { class: 'md small' }, ...mdNodes(t.criteria))) : null,
      h('section', {}, kicker('Workspace'),
        h('dl', { class: 'facts' },
          t.task_kind ? [h('dt', {}, 'Kind'), h('dd', {}, (S.taskKinds.find((k) => k.id === t.task_kind) || {}).label || t.task_kind)] : null,
          t.branch ? [h('dt', {}, 'Branch'), h('dd', { class: 'mono' }, t.branch)] : null,
          t.base_branch ? [h('dt', {}, 'Into'), h('dd', { class: 'mono' }, t.base_branch)] : null,
          t.workspace_path || t.kind === 'chat' ? [h('dt', {}, 'Folder'), h('dd', {}, t.workspace_path ? copyable(t.workspace_path) : h('span', { class: 'faint' }, 'Its own, made with the first message'),
            t.kind === 'chat' ? h('div', { class: 'row-btns' },
              h('button', { class: 'btn xs', onclick: () => changeFolder() }, ic('folder', 12), 'Change…'),
              (t.workspace || '').startsWith('path:') ? h('button', { class: 'btn xs', onclick: () => changeFolder('') }, 'Use its own') : null) : null)] : null,
          [h('dt', {}, 'Started'), h('dd', {}, timeAgo(t.created_at))],
          t.session_id ? [h('dt', {}, 'Session'), h('dd', { class: 'mono faint', title: t.session_id }, t.session_id.slice(0, 12) + '…')] : null)),
      deps.length ? h('section', {}, kicker('Waits for'), deps.map((d) => h('button', { class: 'linkbtn', onclick: () => openThread(d) }, ic('thread', 13), d))) : null,
      t.children?.length ? h('section', {}, kicker('Threads from here'), t.children.map((c) => h('button', { class: 'mini', onclick: () => openThread(c.id) }, h('span', {}, c.title), stamp(c.agent, { small: true }), statusChip(c.status)))) : null,
      t.artifacts?.length ? h('section', {}, kicker('Artifacts'), t.artifacts.map((a) => h('a', { class: 'mini', href: '#/a/' + a.id }, ic('artifact', 13), h('span', {}, a.title), h('small', { class: 'faint' }, a.kind)))) : null,
      contextSection(),
      h('section', {}, kicker('Usage'),
        h('dl', { class: 'facts' },
          h('dt', {}, 'Turns'), h('dd', {}, u.turns || 0),
          h('dt', {}, 'Tokens'), h('dd', {}, num(u.input || 0) + ' in · ' + num(u.output || 0) + ' out'),
          u.cost ? [h('dt', {}, 'Cost'), h('dd', {}, '$' + Number(u.cost).toFixed(2))] : null)),
    );
  }

  /** How full the agent's context is, how near it is to compacting, and when it last did. */
  function contextSection() {
    const c = ctxOf(t);
    if (!c) return h('section', {}, kicker('Context'), h('p', { class: 'faint small' }, t.agent === 'antigravity' ? 'Antigravity doesn’t report this.' : 'Shown after the agent’s first reply.'));
    return h('section', {}, kicker('Context', h('b', { class: 'ctx-pct' + (c.pct >= 85 ? ' hot' : c.pct >= 65 ? ' warm' : '') }, c.pct + '%')),
      h('div', { class: 'ctx-bar', title: `Compacts on its own around ${COMPACT_AT}%` }, h('i', { style: { width: Math.min(100, c.pct) + '%' } }), h('em', { style: { left: COMPACT_AT + '%' } })),
      h('dl', { class: 'facts' },
        h('dt', {}, 'In use'), h('dd', {}, c.window ? `${num(c.used)} of ${num(c.window)} tokens` : num(c.used) + ' tokens'),
        h('dt', {}, 'Compacts'), h('dd', {}, c.pct >= COMPACT_AT ? 'about to' : `around ${COMPACT_AT}% (~${num(Math.round(c.window * COMPACT_AT / 100))})`),
        c.compactions ? [h('dt', {}, 'Compacted'), h('dd', {}, `${c.compactions}×` + (c.compacted_at ? `, last ${ago(c.compacted_at)}` + (c.trigger ? ` (${c.trigger})` : '') : ''))] : null));
  }

  async function changeFolder(to) {
    const f = to ?? (await pickFolder({ title: 'Folder this chat works in', start: t.workspace_path || '' }));
    if (f == null) return;
    try {
      t = { ...t, ...(await patch('threads/' + id, { folder: f })) };
      t = await get('threads/' + id);
      drawSide();
      toast(f ? `Works in ${f} from the next message` : 'Back to a folder of its own');
    } catch (e) {
      toast(e.message, { err: true });
    }
  }

  // ---------------------------------------------------------- decisions
  function drawDecisions() {
    const open = (t.decisions || []).filter((d) => d.answer == null);
    decisionsEl.replaceChildren(...open.map((d) => decisionCard(d, async (i) => {
      try {
        await post('decisions/' + d.id, { choice: i });
        t.decisions = t.decisions.map((x) => (x.id === d.id ? { ...x, answer: i } : x));
        drawDecisions();
        toast('Answer sent');
      } catch (e) {
        toast(e.message, { err: true });
      }
    })));
  }

  // ---------------------------------------------------------- timeline
  async function loadMessages(before) {
    const r = await get(`threads/${id}/messages?limit=80` + (before ? '&before=' + before : ''));
    hasMore = r.messages.length >= 80;
    turns = r.turns;
    return r.messages;
  }
  async function loadActivity() {
    const rows = await get(`threads/${id}/activity`);
    activity = {};
    for (const a of rows) (activity[a.turn_id] ||= []).push(a);
  }

  function drawLog() {
    const atBottom = nearBottom();
    const frag = [];
    if (hasMore) frag.push(h('button', { class: 'more-btn', onclick: loadEarlier }, 'Earlier messages'));
    if (!messages.length) frag.push(emptyThread());
    // the agent's activity goes under the last message of its turn
    const lastOfTurn = {};
    messages.forEach((m) => m.meta?.turn_id && (lastOfTurn[m.meta.turn_id] = m.id));
    let day = '';
    for (const m of messages) {
      const d = dayLabel(m.created_at);
      if (d !== day) {
        day = d;
        frag.push(h('div', { class: 'day' }, h('span', {}, d)));
      }
      frag.push(entry(m, lastOfTurn[m.meta?.turn_id] === m.id ? turnParts(activity[m.meta.turn_id], m.body) : null));
    }
    log.replaceChildren(...frag);
    if (atBottom) toBottom();
  }

  function emptyThread() {
    if (t.kind === 'lead') return empty('agents', 'Talk to the lead', `Say what you want built. ${agentName(t.agent)} plans it, opens threads for the right agents, watches them and reports back here.`);
    if (t.kind === 'chat') return empty('chat', 'A fresh chat', `Ask ${agentName(t.agent)} anything. It works in its own folder, so it can make and run files too.`);
    return empty('thread', 'Nothing yet', 'Messages show up here as the agent starts.');
  }

  async function loadEarlier() {
    const first = messages[0];
    const h0 = scroller.scrollHeight;
    const older = await loadMessages(first?.created_at);
    messages = [...older, ...messages];
    drawLog();
    scroller.scrollTop = scroller.scrollHeight - h0;
  }

  function entry(m, acts) {
    const meta = m.meta || {};
    if (m.role === 'wake') {
      const reason = (m.body.match(/reason="([^"]+)"/) || [])[1] || m.author.toLowerCase();
      const bodyEl = h('pre', { class: 'wake-body', hidden: true }, m.body);
      return h('div', { class: 'wake' + (m.pending ? ' pending' : ''), dataset: { id: m.id } },
        h('button', { class: 'wake-h', onclick: () => (bodyEl.hidden = !bodyEl.hidden) }, ic('bolt', 12), h('span', {}, wakeLabel(reason, m)), m.pending ? h('em', {}, 'queued') : null, h('time', {}, clock(m.created_at))),
        bodyEl);
    }
    if (m.role === 'system') {
      if (meta.artifact) return artifactCard(meta.artifact, m);
      if (meta.decision) {
        const d = (t.decisions || []).find((x) => x.id === meta.decision.id) || meta.decision;
        return h('div', { class: 'sys-entry' }, ic('question', 14), h('div', {}, h('b', {}, 'Asked you: '), m.body, d.answer != null ? h('div', { class: 'answered' }, '→ ', (parseOpts(d.options)[d.answer] || {}).label || 'answered') : h('div', { class: 'faint small' }, 'Waiting for your answer above')));
      }
      const fix = meta.action === 'signin' && meta.agent
        ? h('div', { class: 'sys-acts' },
          h('button', { class: 'btn xs', onclick: () => runStep({ what: 'login', agent: meta.agent, title: `Sign ${agentName(meta.agent)} in again` }).then((code) => code === 0 && act('retry')) }, ic('login', 12), 'Sign in again'),
          h('button', { class: 'btn xs', onclick: () => act('retry') }, ic('refresh', 12), 'Retry'))
        : null;
      return h('div', { class: 'sys-entry' }, ic('info', 14), h('div', { class: 'md' }, ...mdNodes(m.body), fix), h('time', {}, clock(m.created_at)));
    }
    const isAgent = m.role === 'agent';
    const who = isAgent ? meta.agent || t.agent : null;
    const fromLead = !isAgent && /^Lead/.test(m.author);
    const fromBuddy = !isAgent && meta.buddy;
    const files = (meta.files || []).filter(Boolean);
    const e = h('article', { class: 'msg ' + (isAgent ? 'agent' : fromLead ? 'lead-msg' : fromBuddy ? 'lead-msg buddy-msg' : 'you') + (m.pending ? ' pending' : ''), dataset: { id: m.id }, style: isAgent ? { '--ah': agentHue(who) } : null },
      h('header', {},
        isAgent ? stamp(who, { model: meta.model, small: true }) : h('b', { class: 'who' }, m.author || 'You'),
        meta.kickoff ? h('span', { class: 'tag' }, 'kickoff') : null,
        meta.from_thread ? h('button', { class: 'tag link', onclick: () => openThread(meta.from_thread) }, 'from the lead') : null,
        m.pending ? h('span', { class: 'tag' }, 'not read yet') : null,
        m.pending && !isAgent && !meta.kickoff ? pendingActions(m) : null,
        h('time', { title: new Date(m.created_at).toLocaleString() }, clock(m.created_at)),
        h('button', { class: 'ibtn xs msg-more', title: 'Copy', onclick: () => navigator.clipboard?.writeText(m.body).then(() => toast('Copied')), html: icon('copy', 13) })),
      acts?.length ? timeline(acts, steps) : null,
      wireMd(h('div', { class: 'md' }, ...mdNodes(m.body)), openExternal),
      files.length ? h('div', { class: 'files' }, files.map((f) => h('span', { class: 'file-chip' }, ic('clip', 12), String(f).split(/[\\/]/).pop()))) : null);
    return e;
  }

  /** What you can do with a message the agent hasn't read yet: change it, take it back, or have it read right now. */
  function pendingActions(m) {
    const fail = (e) => toast(e.message, { err: true });
    return h('span', { class: 'msg-pend' },
      t.running ? h('button', { class: 'btn xs', title: 'Stop what the agent is doing so it reads this now; it carries on from there', onclick: () => post(`messages/${m.id}/now`).catch(fail) }, ic('bolt', 11), 'Send now') : null,
      h('button', { class: 'ibtn xs', title: 'Edit before it’s read', onclick: async () => {
        const ta = textarea(m.body);
        if (!(await dialog({ title: 'Edit message', body: field('Message', ta), ok: 'Save' }))) return;
        if (ta.value.trim()) patch('messages/' + m.id, { text: ta.value.trim() }).catch(fail);
      }, html: icon('edit', 12) }),
      h('button', { class: 'ibtn xs', title: 'Take it back', onclick: () => del('messages/' + m.id).catch(fail), html: icon('trash', 12) }));
  }

  function wakeLabel(reason, m) {
    const map = {
      'worker-replied': 'A worker reported back',
      'worker-message': 'A thread wrote to you',
      'agent-limit': 'An agent ran out of usage',
      'agent-available': 'An agent is available again',
      'worker-finished': 'A worker finished',
      'person-opened-thread': 'You opened a thread',
      'person-messaged-thread': 'You wrote to a worker',
      'person-merged': 'You merged a branch',
      'decision-answered': 'You answered a decision',
      restarted: 'Model Roamer restarted',
      'tool results': 'Tool results',
      limit: 'An agent hit its limit',
    };
    const th = (m.body.match(/title="([^"]+)"/) || [])[1];
    return (map[reason] || reason.replace(/-/g, ' ')) + (th ? ` · ${th}` : '');
  }

  function artifactCard(a, m) {
    return h('div', { class: 'art-entry cut' },
      ic('artifact', 18),
      h('div', {}, h('b', {}, a.title), h('small', { class: 'faint' }, (a.kind || '') + (a.description ? ' · ' + a.description : ''))),
      h('a', { class: 'btn sm', href: '#/a/' + a.id }, 'Open'),
      h('button', { class: 'ibtn sm', title: 'Open in its own window', onclick: () => popOut('#/a/' + a.id, { key: 'a:' + a.id, w: 1100, h: 820 }), html: icon('popout', 14) }),
      h('time', {}, clock(m.created_at)));
  }

  // ---------------------------------------------------------- live turn
  function drawLive() {
    const on = !!t.running || !!live.turn || !!live.hold;
    liveEl.hidden = !on;
    if (!on) return;
    const full = liveTarget();
    const text = full.slice(0, live.shown);
    if (live.shown < full.length) typeOn();
    const last = live.parts.filter((p) => p.k === 'act').at(-1)?.a;
    const before = trailing() || live.hold ? live.parts.slice(0, trailing() ? -1 : undefined) : live.parts;
    liveEl.replaceChildren(
      h('div', { class: 'live-h', style: { '--ah': agentHue(t.agent) } },
        h('i', { class: 'pulse' }), stamp(t.agent, { model: t.model, small: true }),
        h('span', { class: 'live-what' }, last && steps.mode() !== 'hidden' ? actLabel(last) : text ? 'Writing…' : last ? 'Working…' : 'Thinking…'),
        h('span', { class: 'live-time mono', dataset: { since: live.started || t.running?.started || Date.now() } }, ''),
        h('button', { class: 'btn xs danger', onclick: () => act('stop'), title: 'Stop (Ctrl+.)' }, 'Stop')),
      before.length ? timeline(before, steps) : null,
      text ? wireMd(h('div', { class: 'md live-text' + (live.shown < full.length ? ' typing' : '') }, ...mdNodes(text)), openExternal) : null);
    tick();
    if (nearBottom(220)) toBottom();
  }
  const tick = () => liveEl.querySelectorAll('[data-since]').forEach((x) => (x.textContent = dur(Date.now() - +x.dataset.since)));
  const ticker = setInterval(tick, 1000);

  // ---------------------------------------------------------- composer
  function makeComposer() {
    const ta = h('textarea', { class: 'cmp-in', rows: 1, placeholder: placeholderFor(), spellcheck: 'true' });
    const files = [];
    const chips = h('div', { class: 'cmp-files' });
    const drawChips = () => chips.replaceChildren(...files.map((f, i) => h('span', { class: 'file-chip' + (f.path ? '' : ' up') }, ic('clip', 12), f.name, h('small', {}, f.path ? bytes(f.size) : 'uploading…'), h('button', { title: 'Remove', onclick: () => (files.splice(i, 1), drawChips()), html: icon('close', 11) }))));
    const fileIn = h('input', { type: 'file', multiple: true, hidden: true, onchange: () => (addFiles([...fileIn.files]), (fileIn.value = '')) });
    async function addFiles(list) {
      for (const f of list) {
        const it = { name: f.name, size: f.size };
        files.push(it);
        drawChips();
        try {
          Object.assign(it, await upload(f));
        } catch (e) {
          files.splice(files.indexOf(it), 1);
          toast(`Couldn't attach ${f.name}: ${e.message}`, { err: true });
        }
        drawChips();
      }
    }
    const stepsBtn = h('button', { class: 'cmp-steps', type: 'button', title: 'How this chat shows the agent’s steps', onclick: (e) => stepsMenu(e.currentTarget) });
    const sendBtn = h('button', { class: 'cmp-send', title: 'Send (Enter)', onclick: () => go_(), html: icon('send', 17) });
    async function go_() {
      const text = ta.value.trim();
      if (files.some((f) => !f.path)) return toast('Wait for the attachments to finish uploading');
      if (!text && !files.length) return ta.focus();
      const sent = { text, files: files.map((f) => f.path) };
      ta.value = '';
      files.length = 0;
      drawChips();
      grow();
      try {
        localStorage.removeItem('mr-draft-' + id);
      } catch { /* fine */ }
      await send(sent.text, sent.files, () => {
        ta.value = sent.text;
        grow();
      });
    }
    const grow = () => {
      ta.style.height = 'auto';
      ta.style.height = Math.min(ta.scrollHeight, 280) + 'px';
    };
    ta.oninput = () => {
      grow();
      try {
        localStorage.setItem('mr-draft-' + id, ta.value);
      } catch { /* fine */ }
    };
    ta.onkeydown = (e) => {
      if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
        e.preventDefault();
        go_();
      }
    };
    ta.onpaste = (e) => {
      const fs = [...(e.clipboardData?.files || [])];
      if (fs.length) {
        e.preventDefault();
        addFiles(fs);
      }
    };
    try {
      ta.value = localStorage.getItem('mr-draft-' + id) || '';
    } catch { /* fine */ }
    const el = h('div', { class: 'composer cut', ondragover: (e) => (e.preventDefault(), el.classList.add('drop')), ondragleave: () => el.classList.remove('drop'), ondrop: (e) => (e.preventDefault(), el.classList.remove('drop'), addFiles([...e.dataTransfer.files])) },
      chips,
      h('div', { class: 'cmp-row' },
        h('button', { class: 'ibtn', title: 'Attach files', onclick: () => fileIn.click(), html: icon('clip', 17) }), fileIn,
        ta, sendBtn),
      h('div', { class: 'cmp-hint mono' }, t.kind === 'chat' ? chatPicker() : null, h('span', {}, 'Enter sends · Shift+Enter new line · drop or paste files'), h('span', { class: 'cmp-agent' }), stepsBtn));
    setTimeout(grow);
    return { el, ta, focus: () => ta.focus(), stepsBtn };
  }
  /** A chat's agent and model, changeable any time; the choice carries over to new chats until the app restarts. */
  /** Steps for this chat: follow the setting, or its own choice (remembered with the thread). */
  function stepsMenu(anchor) {
    const r = anchor.getBoundingClientRect();
    const def = S.settings.steps || 'collapsed';
    const set = async (v) => {
      try {
        t = { ...t, ...(await patch('threads/' + id, { steps: v })) };
        redrawSteps();
      } catch (e) {
        toast(e.message, { err: true });
      }
    };
    showMenu([
      { header: 'Steps in this chat' },
      { label: `Use my setting (${def})`, checked: !t.steps, run: () => set('') },
      { sep: true },
      ...STEP_MODES.map(([v, label, title]) => ({ label, title, checked: t.steps === v, run: () => set(v) })),
    ], r.left, r.top - 4, { anchor });
  }
  function drawStepsBtn() {
    const m = steps.mode();
    composer.stepsBtn.replaceChildren(ic(m === 'hidden' ? 'eye' : 'terminal', 12), h('span', {}, 'Steps: ' + m + (t.steps ? '' : '')));
    composer.stepsBtn.classList.toggle('own', !!t.steps);
  }
  function redrawSteps() {
    drawStepsBtn();
    drawLog();
    drawLive();
  }

  function chatPicker() {
    const p = agentModelPicker({ agentId: t.agent, model: t.model || '', onchange: async (a, m) => {
      if (!a) return;
      rememberChatPick(a, m);
      try {
        t = { ...t, ...(await patch('threads/' + id, { agent: a, model: m })) };
        composer.ta.placeholder = placeholderFor();
        drawHead();
      } catch (e) {
        toast(e.message, { err: true });
      }
    } });
    p.el.classList.add('cmp-pick');
    return p.el;
  }
  function placeholderFor() {
    if (t.kind === 'lead') return `Tell the lead (${agentName(t.agent)}) what you want…`;
    if (t.kind === 'chat') return `Message ${agentName(t.agent)}…`;
    return `Write to ${agentName(t.agent)} directly (the lead hears about it)…`;
  }

  async function send(text, files = [], restore) {
    try {
      const m = await post(`threads/${id}/messages`, { text, files });
      if (!messages.some((x) => x.id === m.id)) messages.push(m);
      drawLog();
      toBottom();
    } catch (e) {
      toast(e.message, { err: true });
      restore?.();
    }
  }

  // ---------------------------------------------------------- scrolling
  const nearBottom = (px = 120) => scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < px;
  const toBottom = () => requestAnimationFrame(() => (scroller.scrollTop = scroller.scrollHeight));

  // ---------------------------------------------------------- live events
  const reloadThread = async () => {
    try {
      t = await get('threads/' + id);
      drawHead();
      drawSide();
      drawDecisions();
      drawLive();
    } catch { /* deleted */ }
  };
  let pendingReload = 0;
  const soon = () => (clearTimeout(pendingReload), (pendingReload = setTimeout(reloadThread, 250)));
  let stepsWas = S.settings.steps;
  const unwatchSteps = watch((w) => {
    if ((w === 'settings' || w === 'all') && S.settings.steps !== stepsWas) {
      stepsWas = S.settings.steps;
      redrawSteps();
    }
  });
  const unlisten = listen((ev) => {
    if (ev.kind === 'resync') return refreshAll();
    if (ev.thread !== id) return;
    const d = ev.data || {};
    switch (ev.kind) {
      case 'message': {
        const typingIt = d.role === 'agent' && !reduced && typeof d.body === 'string' && !messages.some((x) => x.id === d.id) && (live.turn || live.shown) && live.shown < d.body.length;
        if (typingIt) {
          // keep what is already on screen if the reply continues it, so typing carries on from there
          if (!d.body.startsWith(liveTarget().slice(0, live.shown))) live.shown = 0;
          live.hold = d;
          drawLive();
          break;
        }
        if (d.deleted) {
          messages = messages.filter((x) => x.id !== d.id);
          drawLog();
          break;
        }
        if (!messages.some((x) => x.id === d.id)) messages.push(d);
        else messages = messages.map((x) => (x.id === d.id ? d : x));
        if (d.role === 'agent') {
          live.parts = [];
          live.shown = 0;
        }
        drawLog();
        drawLive();
        break;
      }
      case 'delta':
        if (live.turn !== d.turn_id) resetLive(d.turn_id, live.started || Date.now());
        liveText(d.block ?? d.text ?? '', d.block != null);
        drawLiveSoon();
        break;
      case 'activity':
        (activity[d.turn_id] ||= []).push(d);
        if (live.turn !== d.turn_id) resetLive(d.turn_id);
        liveStep(d);
        drawLiveSoon();
        break;
      case 'turn':
        if (d.state === 'start') {
          if (live.hold) finishHoldNow();
          resetLive(d.turn_id);
          t.running = { turn: d.turn_id, started: Date.now() };
        } else {
          live.turn = null;
          t.running = null;
          get(`threads/${id}/messages?limit=1`).then((r) => ((turns = r.turns), drawLog())).catch(() => {});
        }
        drawHead();
        drawLive();
        break;
      case 'running':
        if (d.running === false) {
          live.turn = null;
          t.running = null;
          drawLive();
        }
        soon();
        break;
      case 'thread':
        if (d.deleted) return el.replaceChildren(empty('trash', 'This thread was deleted', '', h('button', { class: 'btn', onclick: () => post(`threads/${id}/restore`).then(() => mountThread(el, id, { full, compact, ctx })) }, 'Undo')));
        {
          const before = t.steps;
          t = { ...t, ...d };
          drawHead();
          if (before !== t.steps) redrawSteps();
          else drawLive();
        }
        break;
      case 'decision':
      case 'artifact':
      case 'claims':
        soon();
        break;
      case 'progress':
        t.status_note = d.text;
        drawHead();
        break;
      case 'read': {
        const ids = new Set(d.ids || []);
        messages = messages.map((x) => (ids.has(x.id) ? { ...x, pending: d.pending ? 1 : 0 } : x));
        drawLog();
        break;
      }
    }
  });
  let liveTimer = 0;
  const drawLiveSoon = () => liveTimer || (liveTimer = requestAnimationFrame(() => ((liveTimer = 0), drawLive())));
  // Reveal the next words: a few per frame, more when far behind, so a long block finishes in about a second.
  let typeTimer = 0;
  const reduced = matchMedia('(prefers-reduced-motion: reduce)').matches;
  function typeOn() {
    if (typeTimer) return;
    typeTimer = requestAnimationFrame(() => {
      typeTimer = 0;
      const full = liveTarget();
      if (live.shown > full.length) live.shown = full.length; // a block replaced the streamed text with a shorter one
      const behind = full.length - live.shown;
      if (behind <= 0) return finishHold();
      let next = reduced ? full.length : live.shown + Math.max(3, Math.ceil(behind / 40));
      const space = full.slice(next).search(/\s/); // end on a word boundary
      next = space < 0 ? full.length : next + space;
      live.shown = Math.min(full.length, next);
      drawLive();
      if (live.shown >= full.length) finishHold();
    });
  }
  function finishHoldNow() {
    if (live.hold) live.shown = live.hold.body.length;
    finishHold();
  }
  function finishHold() {
    if (!live.hold || live.shown < live.hold.body.length) return;
    const m = live.hold;
    live.hold = null;
    live.parts = [];
    live.shown = 0;
    if (!messages.some((x) => x.id === m.id)) messages.push(m);
    drawLog();
    drawLive();
  }

  async function refreshAll() {
    Object.assign(live, { hold: null, shown: 0 }); // the reload brings every finished reply
    try {
      [messages] = await Promise.all([loadMessages(), loadActivity()]);
      t = await get('threads/' + id);
      drawHead();
      drawSide();
      drawDecisions();
      drawLog();
      drawLive();
    } catch (e) {
      log.replaceChildren(errorBox(e, refreshAll));
    }
  }

  // ---------------------------------------------------------- keys
  const keys = (e) => {
    if (!el.isConnected) return;
    const k = (e.ctrlKey || e.metaKey) && e.key;
    if (k === '.' && t.running) (e.preventDefault(), act('stop'));
    else if ((e.ctrlKey || e.metaKey) && e.shiftKey && e.key.toLowerCase() === 'o' && !isPop) (e.preventDefault(), popOut('#/t/' + id, { key: 't:' + id }));
  };
  document.addEventListener('keydown', keys);

  drawHead();
  drawStepsBtn();
  log.replaceChildren(loading('Loading the conversation'));
  await refreshAll();
  toBottom();
  // arrived from a search hit: bring that message into view
  try {
    const [jump, at] = (sessionStorage.getItem('mr-jump') || '').split('|');
    if (jump) {
      // a project page mounts several threads: leave the hit for the one that has it, for a few seconds
      if (Date.now() - +at > 10000) sessionStorage.removeItem('mr-jump');
      const m = log.querySelector(`[data-id="${CSS.escape(jump)}"]`);
      if (m) {
        sessionStorage.removeItem('mr-jump');
        m.scrollIntoView({ block: 'center' });
        m.classList.add('found');
        setTimeout(() => m.classList.remove('found'), 2600);
      }
    }
  } catch { /* fine */ }
  if (!compact) composer.focus();
  if (t.unread) patch('threads/' + id, { unread: 0 }).catch(() => {});

  return () => {
    unlisten();
    unwatchSteps();
    clearInterval(ticker);
    document.removeEventListener('keydown', keys);
  };
}

// ------------------------------------------------------------ shared pieces
export function mdNodes(text) {
  const d = h('div', { html: md(text) });
  return [...d.childNodes];
}

function parseOpts(o) {
  if (Array.isArray(o)) return o;
  try {
    return JSON.parse(o || '[]');
  } catch {
    return [];
  }
}

export function decisionCard(d, answer) {
  const opts = parseOpts(d.options);
  return h('div', { class: 'decision cut' },
    h('div', { class: 'dc-h' }, ic('question', 16), h('span', { class: 'eyebrow' }, 'Your call')),
    h('h3', {}, d.question),
    h('div', { class: 'dc-opts' }, opts.map((o, i) => h('button', { class: 'dc-opt' + (i === d.recommended ? ' rec' : ''), onclick: (e) => {
      e.currentTarget.closest('.decision').classList.add('sent');
      answer(i);
    } }, h('b', {}, o.label, i === d.recommended ? h('em', {}, 'recommended') : null), o.consequence ? h('span', {}, o.consequence) : null))));
}

function actLabel(a) {
  if (a.kind === 'tool') return (a.title || 'tool') + (a.body ? ': ' + a.body.split('\n')[0].slice(0, 90) : '');
  if (a.kind === 'thinking') return 'Thinking…';
  if (a.kind === 'error') return 'Error: ' + (a.title || a.body).slice(0, 80);
  return a.title || a.kind;
}

/** A finished turn's steps in order, as timeline parts. The text that became the reply (everything written after
 * the last step, or the same text as the reply) is left out: the reply itself is shown below them. */
export function turnParts(acts, reply) {
  if (!acts?.length) return null;
  const lastStep = acts.map((a) => a.kind !== 'text').lastIndexOf(true);
  const r = (reply || '').trim();
  const parts = acts
    .filter((a, i) => a.kind !== 'text' || (i < lastStep && a.body.trim() && a.body.trim() !== r && !r.startsWith(a.body.trim())))
    .map((a) => (a.kind === 'text' ? { k: 'text', text: a.body } : { k: 'act', a }));
  return parts.length ? parts : null;
}

/** How a thread shows its steps (thinking, tools, commands): its own choice, else the setting, else collapsed. Groups
 * the person opened (or closed, when expanded) are remembered by key, so live redraws keep them as they left them. */
export function stepView(modeOf) {
  const flip = new Set();
  let was = modeOf();
  const mode = () => {
    const m = modeOf();
    if (m !== was) (flip.clear(), (was = m));
    return m;
  };
  return {
    mode,
    open: (key) => (mode() === 'expanded') !== flip.has(key),
    toggle: (key) => (flip.has(key) ? flip.delete(key) : flip.add(key)),
    flipped: (key) => flip.has(key),
  };
}
export const STEP_MODES = [['collapsed', 'Collapsed', 'One line per group of steps; open it to see them'], ['expanded', 'Expanded', 'Every step shown as it happens'], ['hidden', 'Hidden', 'Only what the agent writes to you']];
const actKey = (a) => `${a.turn_id || ''}:${a.created_at || ''}:${a.kind}:${String(a.title || '').slice(0, 24)}`;

/** A turn as it happened: what the agent wrote between steps, its thinking (folded), and runs of tool calls with
 * their results (one strip each). Steps follow the view: collapsed, expanded or hidden; nothing opens by itself. */
export function timeline(parts, view) {
  const hidden = view.mode() === 'hidden';
  const out = [];
  let run = [];
  const flush = () => {
    if (run.length && !hidden) out.push(activityStrip(run, view));
    run = [];
  };
  for (const p of parts) {
    if (p.k === 'text') {
      flush();
      out.push(wireMd(h('div', { class: 'md tl-text' }, ...mdNodes(p.text)), openExternal));
    } else if (p.a.kind === 'thinking') {
      flush();
      if (!hidden) out.push(thinkingBlock(p.a, view));
    } else run.push(p.a);
  }
  flush();
  return out.length ? h('div', { class: 'tl' }, ...out) : null;
}

function thinkingBlock(a, view) {
  const key = 'think:' + actKey(a);
  const text = a.body || '';
  const open = view.open(key);
  const body = wireMd(h('div', { class: 'md tl-think-body', hidden: !open }, ...mdNodes(text)), openExternal);
  const head = h('button', { class: 'tl-think-h' + (open ? ' open' : ''), type: 'button', onclick: () => {
    view.toggle(key);
    body.hidden = !body.hidden;
    head.classList.toggle('open', !body.hidden);
  } }, ic('sparkle', 12), h('span', {}, 'Thinking'), h('span', { class: 'faint' }, text.replace(/\s+/g, ' ').slice(0, 90)), h('i', { class: 'act-more', html: icon('chevDown', 12) }));
  return h('div', { class: 'tl-think' }, head, body);
}

/** The agent's steps in a turn: commands, edits, tool calls and their results. One line until opened (or open, when
 * the view is expanded); a long result stays folded to its first line until clicked. */
export function activityStrip(acts, view) {
  const key = 'run:' + actKey(acts[0]);
  const open = view.open(key);
  const tools = acts.filter((a) => a.kind === 'tool');
  const errors = acts.filter((a) => a.kind === 'error').length;
  const names = [...new Set(tools.map((a) => (a.title || '').split(':')[0]))].slice(0, 5);
  const list = h('div', { class: 'acts', hidden: !open });
  const draw = () => list.replaceChildren(...acts.slice(-200).map((a) => {
    const body = a.body || '';
    const long = body.length > 220 || body.includes('\n');
    const bkey = 'body:' + actKey(a);
    const pre = h('pre', { class: 'act-body', hidden: long && a.kind !== 'thinking' && !view.flipped(bkey) }, body);
    return h('div', { class: 'act k-' + a.kind },
      h('button', { class: 'act-h', onclick: () => long && (view.toggle(bkey), (pre.hidden = !pre.hidden)) },
        h('i', { class: 'act-ic', html: icon(a.kind === 'tool' ? TOOL_ICONS[a.title] || (String(a.title).includes(':') ? 'cpu' : 'bolt') : a.kind === 'result' ? 'right' : a.kind === 'thinking' ? 'sparkle' : a.kind === 'error' ? 'warn' : 'info', 12) }),
        h('span', { class: 'act-t' }, a.kind === 'result' ? 'result' : a.kind === 'thinking' ? 'thinking' : a.title || a.kind),
        !long || a.kind === 'thinking' ? null : h('span', { class: 'act-peek' }, body.split('\n')[0].slice(0, 120)),
        long && a.kind !== 'thinking' ? h('i', { class: 'act-more', html: icon('chevDown', 12) }) : null),
      !long || a.kind === 'thinking' ? (body ? h('div', { class: 'act-short' }, body) : null) : pre);
  }));
  if (open) draw();
  const head = h('button', { class: 'acts-h', onclick: () => {
    view.toggle(key);
    list.hidden = !list.hidden;
    if (!list.hidden && !list.childElementCount) draw();
    head.classList.toggle('open', !list.hidden);
  } }, ic('terminal', 12), h('span', {}, `${tools.length} step${tools.length === 1 ? '' : 's'}`), names.length ? h('span', { class: 'faint' }, names.join(' · ')) : null, errors ? h('span', { class: 'status hot' }, errors + ' error' + (errors > 1 ? 's' : '')) : null, h('i', { class: 'act-more', html: icon('chevDown', 12) }));
  if (open) head.classList.add('open');
  return h('div', { class: 'acts-wrap' }, head, list);
}

export function renderDiff(text) {
  const box = h('div', { class: 'diff mono' });
  const lines = text.split('\n').slice(0, 6000);
  const files = [];
  let cur = null;
  for (const l of lines) {
    if (l.startsWith('diff --git')) {
      cur = { name: l.replace(/^diff --git a\/(.*?) b\/.*$/, '$1'), lines: [], add: 0, rem: 0 };
      files.push(cur);
      continue;
    }
    if (!cur) continue;
    if (/^(index |--- |\+\+\+ |new file|deleted file|similarity|rename )/.test(l)) continue;
    if (l.startsWith('+')) cur.add++;
    else if (l.startsWith('-')) cur.rem++;
    cur.lines.push(l);
  }
  if (!files.length) return h('pre', { class: 'diff mono' }, text);
  for (const f of files) {
    const body = h('div', { class: 'df-body' }, f.lines.map((l) => h('div', { class: l.startsWith('@@') ? 'hunk' : l.startsWith('+') ? 'add' : l.startsWith('-') ? 'rem' : 'ctx' }, l || ' ')));
    box.append(h('details', { class: 'df', open: files.length < 12 }, h('summary', {}, h('span', {}, f.name), h('b', { class: 'add' }, '+' + f.add), h('b', { class: 'rem' }, '−' + f.rem)), body));
  }
  return box;
}

export { statusLabel, ago, native, isDesktop, projectOf };
