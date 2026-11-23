// Agents: each harness CLI with its install, sign-in, test and settings; the helper tools; and the routing rules
// that say which agent gets which kind of work.

import { h, ago, until } from '../util.js';
import { icon } from '../icons.js';
import { get, post, patch, del, listen } from '../api.js';
import { S, watch, setSetting, agentName, agentHue, agentList } from '../store.js';
import { go } from '../app.js';
import { openExternal } from '../native.js';
import { dialog, toast, showMenu } from '../ui.js';
import { runStep } from '../term.js';
import { ic, stamp, field, input, textarea, select, seg, toggle, modelOptions, agentOptions, loading, errorBox, empty, kicker, timeAgo } from '../common.js';

const TABS = [['agents', 'Agents', 'agents'], ['tools', 'Tools', 'terminal'], ['routing', 'Routing', 'route']];

export default async function agents(el, r) {
  const tab = r.q.tab || 'agents';
  const body = h('div');
  el.append(
    h('div', { class: 'page-h' }, h('div', {}, h('h1', {}, 'Agents'), h('p', { class: 'lede' }, 'The harnesses Model Roamer drives. Install, sign in and test each one here; no terminal needed.')),
      h('button', { class: 'btn', onclick: () => refresh(true) }, ic('refresh', 14), 'Check again')),
    h('nav', { class: 'p-tabs' }, TABS.map(([k, label, ico]) => h('a', { class: 'p-tab' + (k === tab ? ' on' : ''), href: '#/agents?tab=' + k }, ic(ico, 14), label))),
    body);
  let un = () => {};
  async function refresh(force) {
    if (force) {
      body.querySelectorAll('.ag-state').forEach((x) => x.replaceChildren(h('span', { class: 'spin' }), 'Checking…'));
      try {
        await get('agents?refresh=1', { timeout: 90000 });
      } catch (e) {
        toast(e.message, { err: true });
      }
    }
  }
  if (tab === 'agents') un = await agentCards(body, r.q.open);
  else if (tab === 'tools') un = await tools(body);
  else un = await routing(body);
  return un;
}

// ------------------------------------------------------------ agent cards
let checkingAll = null;
async function agentCards(el, openId) {
  const grid = h('div', { class: 'ag-grid' });
  el.append(grid, h('div', { class: 'ag-custom' }, kicker('Your own agents'), h('p', { class: 'faint small' }, 'Any command-line agent that takes a prompt and prints an answer can join the fleet.'), h('button', { class: 'btn', onclick: () => customDialog() }, ic('plus', 13), 'Add an agent')));
  const draw = () => grid.replaceChildren(...agentList().map((a) => card(a)));
  draw();
  // the app checks every agent when it starts; ask only if none of that has come back (once per visit, never stacked)
  if (agentList().every((a) => a.pending) && !checkingAll) {
    checkingAll = get('agents?refresh=1', { timeout: 90000 }).catch(() => {}).finally(() => (checkingAll = null));
  }
  if (openId) setTimeout(() => document.getElementById('ag-' + openId)?.scrollIntoView({ block: 'center', behavior: 'smooth' }), 100);
  return watch((w) => (w === 'agents' || w === 'all' || w === 'settings') && draw());
}

function authOf(a) {
  if (a.pending) return ['Checking…', 'dim', 'spin'];
  if (!a.installed) return ['Not installed', 'faint', 'download'];
  if (a.auth?.signed_in === 'yes') return [a.auth.detail || 'Signed in', 'acc', 'check'];
  if (a.auth?.signed_in === 'no') return [a.auth.detail || 'Not signed in', 'hot', 'login'];
  return [a.auth?.detail || 'Sign-in unknown (try Test)', 'dim', 'question'];
}

function card(a) {
  const info = a.info || {};
  const cfg = a.config || {};
  const [authText, authTone, authIco] = authOf(a);
  const busy = Object.keys(S.running).length;
  const lim = a.limit?.active ? a.limit : null;
  const steps = a.install || [];
  const testOut = h('div', { class: 'ag-test' });
  const settings = h('div', { class: 'ag-settings', hidden: true });

  const actions = [];
  if (!a.installed && !a.pending) {
    actions.push(h('button', { class: 'btn primary sm', onclick: (e) => (steps.length > 1 ? stepMenu(e.currentTarget, a, steps, 'install') : runStep({ what: 'install', agent: a.id, title: steps[0]?.label })) }, ic('download', 13), 'Install'));
  } else if (a.installed) {
    if (a.auth?.signed_in !== 'yes') actions.push(h('button', { class: 'btn primary sm', onclick: () => runStep({ what: 'login', agent: a.id, title: a.login?.label }) }, ic('login', 13), 'Sign in'));
    actions.push(h('button', { class: 'btn sm', onclick: () => test(a, testOut) }, ic('play', 13), 'Test'));
  }
  const more = h('button', { class: 'ibtn sm', title: 'More', onclick: (e) => {
    const r = e.currentTarget.getBoundingClientRect();
    showMenu([
      a.installed ? { label: 'Sign in again', icon: 'login', run: () => runStep({ what: 'login', agent: a.id, title: a.login?.label }) } : null,
      a.installed && a.logout ? { label: 'Sign out', icon: 'logout', run: () => runStep({ what: 'logout', agent: a.id, title: a.logout?.label }) } : null,
      a.installed ? { label: 'Update', icon: 'download', run: () => runStep({ what: 'update', agent: a.id }) } : null,
      { label: a.installed ? 'Reinstall…' : 'Install with…', icon: 'download', sub: steps.map((s, i) => ({ label: s.label, run: () => runStep({ what: 'install', agent: a.id, step: i, title: s.label }) })) },
      info.mcp === false ? null : { label: 'Add Model Roamer tools to its own config', icon: 'cpu', title: 'Not needed for Model Roamer itself; lets you use the tools from the CLI by hand', run: () => runStep({ what: 'connect', agent: a.id, title: 'Connect Model Roamer tools' }) },
      { sep: true },
      { label: 'Check again', icon: 'refresh', run: () => get('agents/' + a.id, { timeout: 60000 }).catch((e) => toast(e.message, { err: true })) },
      a.installed ? { label: 'Refresh models', icon: 'refresh', title: 'Ask the CLI which models it offers now', run: () => post(`agents/${a.id}/models`, {}, { timeout: 90000 }).then((r) => toast(`${(r.info?.models || []).filter((m) => m.id).length} models from ${info.name}`)).catch((e) => toast(e.message, { err: true })) } : null,
      info.site ? { label: 'Website', icon: 'external', run: () => openExternal(info.site) } : null,
      a.id.match(/^(claude|codex|antigravity)$/) ? null : { label: 'Remove this agent', icon: 'trash', danger: true, run: async () => (await del('agents/' + a.id), toast('Removed'), location.reload()) },
    ], r.right, r.bottom + 4, { anchor: e.currentTarget });
  }, html: icon('more', 15) });

  const on = cfg.enabled !== false;
  return h('article', { class: 'ag-card cut' + (on ? '' : ' off'), id: 'ag-' + a.id, style: { '--ah': info.hue } },
    h('div', { class: 'ag-top' },
      h('i', { class: 'ag-sw' }),
      h('div', { class: 'ag-name' }, h('h3', {}, info.name), h('small', {}, info.vendor)),
      toggle(on, (v) => patch('agents/' + a.id, { enabled: v }).then(() => toast(v ? `${info.name} is on` : `${info.name} is off: no new work goes to it`)), { title: 'Use this agent' })),
    h('p', { class: 'ag-sum' }, info.summary),
    h('div', { class: 'ag-tags' }, (info.strengths || []).map((s) => h('span', { class: 'tag' }, s)), info.can_lead ? h('span', { class: 'tag acc' }, 'can lead') : null),
    h('dl', { class: 'facts ag-facts' },
      h('dt', {}, 'Program'), h('dd', { class: 'mono' }, a.installed ? [a.version || 'installed', a.path ? h('small', { class: 'faint', title: a.path }, ' · ' + a.path) : null] : a.pending ? 'checking…' : 'not found'),
      h('dt', {}, 'Account'), h('dd', { class: 'ag-state' }, h('span', { class: 'status ' + authTone }, authIco === 'spin' ? h('span', { class: 'spin' }) : null, authText),
        a.checked_at && !a.pending ? h('small', { class: 'faint', title: 'Remembered between launches. More › Check again looks now.' }, ' · checked ', timeAgo(a.checked_at)) : null),
      lim ? [h('dt', {}, 'Limit'), h('dd', {}, h('span', { class: 'status hot' }, lim.until ? `At its limit, back ${until(lim.until)}` : 'At its usage limit'), h('button', { class: 'linkbtn small', onclick: () => del(`agents/${a.id}/limit`).then(() => toast('Cleared: queued work goes to it again')) }, 'It’s back, clear it'))] : null,
      a.pty ? [h('dt', {}, 'Needs'), h('dd', { class: 'faint small' }, 'a terminal (ConPTY on Windows), handled for you')] : null),
    testOut,
    h('div', { class: 'ag-acts' }, actions, h('span', { class: 'grow' }), h('button', { class: 'btn sm', onclick: (e) => {
      settings.hidden = !settings.hidden;
      e.currentTarget.classList.toggle('on', !settings.hidden);
      if (!settings.hidden && !settings.childElementCount) drawSettings(settings, a);
    } }, ic('settings', 13), 'Settings'), more),
    settings);
}

function stepMenu(anchor, a, steps, what) {
  const r = anchor.getBoundingClientRect();
  showMenu([{ header: 'How to install' }, ...steps.map((s, i) => ({ label: s.label, kbd: s.needs?.length ? 'needs ' + s.needs.join(', ') : '', run: () => runStep({ what, agent: a.id, step: i, title: s.label }) }))], r.left + 250, r.bottom + 4, { anchor });
}

async function test(a, out) {
  out.replaceChildren(h('div', { class: 'status dim' }, h('span', { class: 'spin' }), `Asking ${a.info.name} to say “ROAMER READY”…`));
  try {
    const r = await post(`agents/${a.id}/test`, {}, { timeout: 180000 });
    out.replaceChildren(h('div', { class: 'ag-test-r ' + (r.ok ? 'ok' : 'bad') },
      h('b', {}, r.ok ? `Works (${r.seconds}s)` : r.limit ? 'At its usage limit' : 'Didn’t answer as expected'),
      r.reply ? h('pre', {}, r.reply) : null,
      r.limit ? h('pre', {}, r.limit) : null,
      r.error ? h('pre', { class: 'hot' }, r.error) : null,
      !r.ok && !r.limit ? h('small', { class: 'faint' }, 'Usually a sign-in: try Sign in, then Test again.') : null));
  } catch (e) {
    out.replaceChildren(errorBox(e));
  }
}

function drawSettings(el, a) {
  const cfg = a.config || {};
  const save = (p) => patch('agents/' + a.id, p).then(() => toast('Saved', { ms: 1200 })).catch((e) => toast(e.message, { err: true }));
  // effort levels come from the CLI's model list when it gives them (they differ per model), else a common set
  const EFFORT = { minimal: 'Minimal', low: 'Low', medium: 'Medium', high: 'High', xhigh: 'Extra high', max: 'Max', ultra: 'Ultra' };
  const effortsFor = (model) => {
    const ms = a.info?.models || [];
    const m = ms.find((x) => x.id === model);
    const lv = m?.efforts?.length ? m.efforts : [...new Set(ms.flatMap((x) => x.efforts || []))];
    const list = lv.length ? lv : a.id === 'claude' ? ['low', 'medium', 'high', 'xhigh', 'max'] : ['low', 'medium', 'high'];
    return [['', 'CLI default'], ...list.map((e) => [e, EFFORT[e] || e])];
  };
  const effortBox = h('span');
  const drawEffort = (model) => effortBox.replaceChildren(select(effortsFor(model), cfg.effort || '', (v) => save({ effort: (cfg.effort = v) })));
  drawEffort(cfg.model || '');
  const path = input(cfg.path || '', { placeholder: 'Found automatically. Or the full path to the program.' });
  path.onchange = () => save({ path: path.value.trim() });
  const args = input((cfg.extra_args || []).join(' '), { placeholder: 'Extra arguments for every turn (advanced)' });
  args.onchange = () => save({ extra_args: args.value.trim() ? args.value.trim().split(/\s+/) : [] });
  const par = input(cfg.max_parallel ?? 3, { type: 'number', min: 1, max: 12, class: 'input num' });
  par.onchange = () => save({ max_parallel: Math.max(1, Math.min(12, +par.value || 1)) });
  el.replaceChildren(
    field('Model', select(modelOptions(a.id), cfg.model || '', (v) => (save({ model: v }), drawEffort(v))), 'Used for new work unless a thread names another. The list comes from the CLI itself: More › Refresh models asks it again.'),
    a.id === 'claude' || a.id === 'codex' ? field('Effort', effortBox) : null,
    field('Permissions', seg([['full', 'Full auto'], ['edits', 'Edits only'], ['readonly', 'Read only']], cfg.autonomy || 'full', (v) => save({ autonomy: v })), 'Full auto lets it run commands without asking (it works in its own worktree). Read only is for reviews and research.'),
    field('At the same time', par, 'How many turns of this agent may run at once. Subscriptions have limits; lower is gentler on them.'),
    field('Program', path),
    field('Extra arguments', args));
}

// ------------------------------------------------------------ custom agent
async function customDialog() {
  const id = input('', { placeholder: 'aider' });
  const name = input('', { placeholder: 'Aider' });
  const command = input('', { placeholder: 'aider' });
  const args = input('--message {prompt} --yes', { placeholder: 'Arguments; {prompt} {model} {cwd} {session} are filled in' });
  let via = 'arg', output = 'text', lead = false;
  const ok = await dialog({
    title: 'Add an agent',
    wide: true,
    body: h('div', { class: 'dlg-fields' },
      h('div', { class: 'row2' }, field('Id', id), field('Name', name)),
      field('Command', command),
      field('Arguments', args),
      h('div', { class: 'row2' },
        field('The prompt goes', seg([['arg', 'In the arguments'], ['stdin', 'To its input']], via, (v) => (via = v))),
        field('It prints', seg([['text', 'Plain text'], ['claude', 'Claude JSON'], ['codex', 'Codex JSON'], ['gemini', 'Gemini JSON']], output, (v) => (output = v)))),
      toggle(false, (v) => (lead = v), { label: 'It can lead a project' })),
    ok: 'Add agent',
    check: async () => {
      if (!id.value.trim() || !command.value.trim()) return 'An id and a command are needed.';
      try {
        await post('agents', { id: id.value.trim(), name: name.value.trim() || id.value.trim(), command: command.value.trim(), args: args.value.trim() ? args.value.trim().match(/(?:[^\s"]+|"[^"]*")+/g).map((x) => x.replace(/^"|"$/g, '')) : [], prompt_via: via, output, can_lead: lead });
      } catch (e) {
        return e.message;
      }
    },
  });
  if (ok) location.reload();
}

// ------------------------------------------------------------ tools
async function tools(el) {
  el.replaceChildren(loading('Looking for the tools'));
  async function draw() {
    let st;
    try {
      st = await get('setup', { timeout: 60000 });
    } catch (e) {
      return el.replaceChildren(errorBox(e, draw));
    }
    el.replaceChildren(
      h('p', { class: 'lede small' }, `Helpers the agents lean on, on ${st.os === 'windows' ? 'Windows' : st.os === 'macos' ? 'macOS' : 'Linux'}${st.headless ? ' (this server)' : ''}.`),
      h('div', { class: 'tool-list' }, st.tools.map((t) => h('div', { class: 'tool-row cut' },
        h('div', { class: 'tr-main' }, h('b', {}, t.name), h('small', { class: 'faint' }, t.why)),
        h('span', { class: 'status ' + (t.installed ? 'acc' : 'faint') }, t.installed ? t.version || 'Installed' : 'Not installed'),
        !t.installed && t.install?.length ? h('button', { class: 'btn sm primary', onclick: async (e) => {
          if (t.install.length > 1) {
            const r = e.currentTarget.getBoundingClientRect();
            showMenu(t.install.map((s, i) => ({ label: s.label, run: () => runStep({ what: 'tool', agent: t.id, step: i, title: s.label }).then(draw) })), r.right, r.bottom + 4);
          } else {
            await runStep({ what: 'tool', agent: t.id, title: t.install[0].label });
            draw();
          }
        } }, ic('download', 13), 'Install') : null,
        t.installed && t.login ? h('button', { class: 'btn sm', onclick: () => runStep({ what: 'tool-login', agent: t.id, title: t.login.label }).then(draw) }, ic('login', 13), t.login.label) : null))),
      h('p', { class: 'faint small' }, 'Installs run in a terminal panel. Some ask for your password (sudo) or a confirmation; type it there.'));
  }
  await draw();
  return listen((ev) => ev.kind === 'setup' && draw());
}

// ------------------------------------------------------------ routing
async function routing(el) {
  const draw = () => {
    const rt = S.settings.routing || {};
    const fall = S.settings.lead_fallback || [];
    el.replaceChildren(
      h('section', { class: 'panel cut' }, kicker('Who gets what', h('small', { class: 'faint' }, 'the lead follows this when it opens threads; projects can override it')),
        h('div', { class: 'route-grid' }, S.taskKinds.map((k) => h('div', { class: 'route-row' },
          h('span', {}, k.label),
          h('span', { class: 'mono faint small' }, k.id),
          select(agentOptions(), rt[k.id] || 'claude', (v) => setSetting({ routing: { [k.id]: v } }).then(() => toast('Saved', { ms: 1000 }))),
          stamp(rt[k.id] || 'claude', { small: true }))))),
      h('section', { class: 'panel cut' }, kicker('Leading'),
        field('New projects are led by', select(agentOptions({ leadOnly: true }), S.settings.default_lead, (v) => setSetting({ default_lead: v, default_lead_model: '' }).then(draw))),
        field('With the model', select(modelOptions(S.settings.default_lead || 'claude'), S.settings.default_lead_model || '', (v) => setSetting({ default_lead_model: v }))),
        field('If the lead hits its limit, hand over to', h('div', { class: 'fallback' }, fall.map((a, i) => h('span', { class: 'fb-i' }, h('b', {}, i + 1), stamp(a, { small: true }),
          i > 0 ? h('button', { class: 'ibtn xs', title: 'Earlier', onclick: () => {
            const f = [...fall];
            [f[i - 1], f[i]] = [f[i], f[i - 1]];
            setSetting({ lead_fallback: f }).then(draw);
          }, html: icon('left', 12) }) : null)))),
        toggle(S.settings.auto_fallback === true, (v) => setSetting({ auto_fallback: v }), { label: 'When an agent runs out of usage, move its work to another agent without asking (otherwise you are asked: move it, or wait for the reset)' }),
        toggle(S.settings.cross_review !== false, (v) => setSetting({ cross_review: v }), { label: 'Have a different agent review finished work before it’s merged' })),
      h('section', { class: 'panel cut' }, kicker('Turns'),
        h('div', { class: 'row3' },
          numField('Stop a turn that’s silent for (minutes)', 'idle_timeout_min', 3, 240),
          numField('Longest turn (minutes)', 'max_turn_min', 10, 1440),
          numField('Tool rounds per turn (text tools)', 'max_tool_rounds', 1, 40))));
  };
  const numField = (label, key, min, max) => {
    const i = input(S.settings[key], { type: 'number', min, max, class: 'input num' });
    i.onchange = () => setSetting({ [key]: Math.max(min, Math.min(max, +i.value || min)) }).then(() => toast('Saved', { ms: 1000 }));
    return field(label, i);
  };
  draw();
  return watch((w) => w === 'agents' && draw());
}

export { ago, agentName, agentHue, go, textarea, empty };
