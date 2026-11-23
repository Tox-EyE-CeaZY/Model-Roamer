// Usage: turns and tokens per agent per day, limits hit, recent turns. Subscriptions (Claude, ChatGPT, Google) report
// tokens rather than money, so tokens and turns lead; cost shows only when an agent reports one.

import { h, ago, num, dur } from '../util.js';
import { get, del, listen } from '../api.js';
import { S, agentName } from '../store.js';
import { openThread } from '../app.js';
import { toast } from '../ui.js';
import { ic, stamp, seg, loading, errorBox, empty, kicker, statusChip } from '../common.js';

// Series colours, validated for colour-blind separation against the night and paper surfaces (OKLCH L, C, hue).
// Identity never rests on colour alone: there is always a legend and the table below the chart.
const SERIES = {
  dark: { claude: [0.66, 0.16, 45], codex: [0.6, 0.12, 175], antigravity: [0.64, 0.16, 330] },
  light: { claude: [0.62, 0.16, 45], codex: [0.56, 0.11, 175], antigravity: [0.6, 0.17, 330] },
};
const ORDER = ['claude', 'codex', 'antigravity'];
function color(agent) {
  const mode = document.documentElement.dataset.theme === 'paper' ? 'light' : 'dark';
  const c = SERIES[mode][agent];
  return c ? `oklch(${c[0]} ${c[1]} ${c[2]})` : 'oklch(.6 0 0)'; // any other agent: neutral "Other"
}

export default async function usage(el, r) {
  let days = +(r.q.days || 14);
  let metric = 'turns';
  const body = h('div');
  el.append(
    h('div', { class: 'page-h' }, h('div', {}, h('h1', {}, 'Usage'), h('p', { class: 'lede' }, 'How hard each agent is working, and when one ran into its plan’s limit.')),
      seg([[7, '7 days'], [14, '14 days'], [30, '30 days'], [90, '90 days']], days, (v) => ((days = +v), load()))),
    body);
  let data;
  // each plan's 5-hour and weekly use: the same figures as the status bar, which a phone doesn't have
  const plans = h('section', { class: 'panel cut plans' });
  body.before(plans);
  async function loadPlans() {
    try {
      const a = await get('activity', { quiet: true, timeout: 15000 });
      const pct = (w) => (w && typeof w.used === 'number' ? Math.round(w.used * 100) : null);
      const reset = (w) => (w?.resets_at ? 'resets ' + new Date(w.resets_at).toLocaleString(undefined, { weekday: 'short', hour: 'numeric', minute: '2-digit' }) : '');
      const bar = (label, w, lim) => {
        const v = lim ? 100 : pct(w);
        return h('div', { class: 'plan-w' + (v >= 90 ? ' hot' : '') },
          h('span', { class: 'plan-l' }, label),
          h('span', { class: 'sb-m-bar wide' }, h('i', { style: { width: (v ?? 0) + '%' } })),
          h('b', {}, v == null ? 'unknown' : lim ? 'OUT' : v + '%'),
          h('small', { class: 'faint' }, reset(w)));
      };
      plans.replaceChildren(kicker('Plan limits', h('small', { class: 'faint' }, 'how much of each plan is used right now')),
        ...(a.quotas || []).map((q) => h('div', { class: 'plan-row', style: { '--ah': q.hue ?? 200, '--m': `oklch(.72 .16 ${q.hue ?? 200})` } },
          h('b', { class: 'plan-n' }, q.name),
          bar('5-hour', q.five_hour, q.limit?.active), bar('Weekly', q.weekly, false),
          h('small', { class: 'faint plan-f' }, `${q.turns_5h || 0} turns in 5 hours, ${q.turns_7d || 0} this week` + (q.at ? ` · checked ${dur(Date.now() - q.at)} ago` : '')))));
    } catch { /* the figures just stay as they were */ }
  }
  loadPlans();
  const planTimer = setInterval(loadPlans, 30000);
  async function load() {
    body.replaceChildren(loading());
    try {
      data = await get('usage?days=' + days);
      draw();
    } catch (e) {
      body.replaceChildren(errorBox(e, load));
    }
  }
  function draw() {
    const tot = data.totals.reduce((a, t) => ({ turns: a.turns + t.turns, input: a.input + (t.input || 0), output: a.output + (t.output || 0), cost: a.cost + (t.cost || 0), failed: a.failed + (t.failed || 0), limited: a.limited + (t.limited || 0) }), { turns: 0, input: 0, output: 0, cost: 0, failed: 0, limited: 0 });
    const active = (data.limits || []).filter((l) => !l.limited_until || l.limited_until > Date.now());
    const chartBox = h('div', { class: 'chart-box' });
    body.replaceChildren(
      h('div', { class: 'tiles' },
        tile('Turns', num(tot.turns), `in ${days} days`),
        tile('Tokens in', num(tot.input), 'prompt and context'),
        tile('Tokens out', num(tot.output), 'written by agents'),
        tot.cost ? tile('Reported cost', '$' + tot.cost.toFixed(2), 'from agents that report it') : null,
        tile('Failed', tot.failed, tot.turns ? Math.round((tot.failed / tot.turns) * 100) + '% of turns' : '—', tot.failed ? 'hot' : ''),
        tile('Limits hit', tot.limited, active.length ? active.length + ' active now' : 'none active', active.length ? 'hot' : '')),
      active.length ? h('section', { class: 'panel cut limits' }, kicker('At their limit now'), active.map((l) => h('div', { class: 'lim-row' }, stamp(l.agent, { small: true }), h('span', {}, l.note || 'Usage limit'), h('small', { class: 'faint' }, l.limited_until ? 'until ' + new Date(l.limited_until).toLocaleString() : 'reset time unknown'), h('button', { class: 'btn xs', onclick: () => del(`agents/${l.agent}/limit`).then(() => (toast('Cleared'), load())) }, 'Clear')))) : null,
      h('section', { class: 'panel cut' }, h('div', { class: 'chart-h' }, kicker('Per day'), seg([['turns', 'Turns'], ['output', 'Tokens out'], ['input', 'Tokens in']], metric, (v) => ((metric = v), chart(chartBox)))), chartBox),
      h('section', { class: 'panel cut' }, kicker('Per agent'), agentTable()),
      h('section', { class: 'panel cut' }, kicker('Recent turns'), recent()));
    chart(chartBox);
  }
  function tile(label, value, sub, tone = '') {
    return h('div', { class: 'tile cut ' + tone }, h('span', { class: 'eyebrow' }, label), h('b', {}, value), h('small', {}, sub));
  }
  function chart(box) {
    const offset = data.tz_offset_ms || 0;
    const today = Math.floor((Date.now() + offset) / 86400000);
    const dayList = Array.from({ length: days }, (_, i) => today - days + 1 + i);
    const agents = [...new Set(data.per_day.map((d) => d.agent))].sort((a, b) => (ORDER.indexOf(a) + 99 * (ORDER.indexOf(a) < 0)) - (ORDER.indexOf(b) + 99 * (ORDER.indexOf(b) < 0)));
    if (!agents.length) return box.replaceChildren(empty('chart', 'No turns yet', 'Once agents start working, their turns and tokens show up here per day.'));
    const val = (d) => +(d[metric] || 0);
    const cells = dayList.map((day) => ({ day, rows: agents.map((a) => ({ a, v: val(data.per_day.find((d) => d.day === day && d.agent === a) || {}) })) }));
    const max = Math.max(1, ...cells.map((c) => c.rows.reduce((s, r) => s + r.v, 0)));
    const W = 760, H = 220, padL = 44, padB = 22, padT = 8;
    const bw = (W - padL) / days;
    const barW = Math.max(3, Math.min(28, bw - 4));
    const y = (v) => H - padB - ((H - padB - padT) * v) / max;
    const ticks = niceTicks(max);
    const svg = [`<svg viewBox="0 0 ${W} ${H}" class="chart" role="img" aria-label="${metric} per day by agent">`];
    for (const t of ticks) svg.push(`<line x1="${padL}" x2="${W}" y1="${y(t)}" y2="${y(t)}" class="grid"/><text x="${padL - 6}" y="${y(t) + 3}" text-anchor="end" class="axis">${num(t)}</text>`);
    cells.forEach((c, i) => {
      const x = padL + i * bw + (bw - barW) / 2;
      let base = 0;
      const parts = c.rows.filter((r) => r.v > 0);
      parts.forEach((r, j) => {
        const y0 = y(base), y1 = y(base + r.v);
        const top = j === parts.length - 1;
        // a 2px surface gap between stacked segments
        const hgt = Math.max(1, y0 - y1 - (j > 0 ? 2 : 0));
        svg.push(top ? `<path d="${roundTop(x, y1, barW, hgt)}" fill="${color(r.a)}"/>` : `<rect x="${x}" y="${y1}" width="${barW}" height="${hgt}" fill="${color(r.a)}"/>`);
        base += r.v;
      });
      const label = new Date(c.day * 86400000 - offset + 43200000);
      if (days <= 14 || i % Math.ceil(days / 10) === 0) svg.push(`<text x="${x + barW / 2}" y="${H - 6}" text-anchor="middle" class="axis">${label.getDate()}</text>`);
      svg.push(`<rect x="${padL + i * bw}" y="${padT}" width="${bw}" height="${H - padB - padT}" class="hit" data-i="${i}"/>`);
    });
    svg.push(`<line x1="${padL}" x2="${W}" y1="${H - padB}" y2="${H - padB}" class="base"/></svg>`);
    const tip = h('div', { class: 'tip', hidden: true });
    const legend = h('div', { class: 'legend' }, agents.map((a) => h('span', {}, h('i', { style: { background: color(a) } }), agentName(a))));
    const wrap = h('div', { class: 'chart-wrap', html: svg.join('') });
    wrap.append(tip);
    wrap.addEventListener('pointermove', (e) => {
      const hit = e.target.closest?.('.hit');
      if (!hit) return (tip.hidden = true);
      const c = cells[+hit.dataset.i];
      const d = new Date(c.day * 86400000 - offset + 43200000);
      tip.replaceChildren(h('b', {}, d.toLocaleDateString(undefined, { weekday: 'short', month: 'short', day: 'numeric' })), ...c.rows.filter((r) => r.v).map((r) => h('div', {}, h('i', { style: { background: color(r.a) } }), h('span', {}, agentName(r.a)), h('b', {}, num(r.v)))), c.rows.every((r) => !r.v) ? h('div', { class: 'faint' }, 'Nothing') : null);
      tip.hidden = false;
      const rect = wrap.getBoundingClientRect();
      tip.style.left = Math.min(rect.width - 170, e.clientX - rect.left + 12) + 'px';
      tip.style.top = Math.max(0, e.clientY - rect.top - 20) + 'px';
      wrap.querySelectorAll('.hit.on').forEach((x) => x.classList.remove('on'));
      hit.classList.add('on');
    });
    wrap.addEventListener('pointerleave', () => ((tip.hidden = true), wrap.querySelectorAll('.hit.on').forEach((x) => x.classList.remove('on'))));
    box.replaceChildren(legend, wrap);
  }
  function agentTable() {
    if (!data.totals.length) return h('p', { class: 'faint small' }, 'Nothing yet.');
    return h('div', { class: 'table-wrap' }, h('table', { class: 'tbl' },
      h('thead', {}, h('tr', {}, ['Agent', 'Turns', 'Tokens in', 'Tokens out', 'Failed', 'Limits', 'Last turn'].map((x) => h('th', {}, x)))),
      h('tbody', {}, data.totals.map((t) => h('tr', {}, h('td', {}, h('span', { class: 'lg-dot', style: { background: color(t.agent) } }), agentName(t.agent)), h('td', {}, num(t.turns)), h('td', {}, num(t.input)), h('td', {}, num(t.output)), h('td', { class: t.failed ? 'hot' : '' }, t.failed || 0), h('td', { class: t.limited ? 'hot' : '' }, t.limited || 0), h('td', { class: 'faint' }, ago(t.last)))))));
  }
  function recent() {
    if (!data.recent.length) return h('p', { class: 'faint small' }, 'No turns yet.');
    return h('div', { class: 'table-wrap' }, h('table', { class: 'tbl' },
      h('thead', {}, h('tr', {}, ['Thread', 'Agent', 'Status', 'Tokens', 'Took', 'When'].map((x) => h('th', {}, x)))),
      h('tbody', {}, data.recent.map((t) => h('tr', { class: 'clickable', onclick: () => t.thread_id && openThread(t.thread_id) },
        h('td', {}, t.title || '(deleted)'), h('td', {}, agentName(t.agent) + (t.model ? ' · ' + t.model : '')), h('td', {}, statusChip(t.status === 'running' ? 'working' : t.status)),
        h('td', {}, num(t.input_tokens) + ' / ' + num(t.output_tokens)), h('td', {}, t.ended_at ? dur(t.ended_at - t.started_at) : '…'), h('td', { class: 'faint' }, ago(t.started_at)))))));
  }
  await load();
  const off = listen((ev) => (ev.kind === 'turn' && ev.data?.state === 'end' && load()) || (ev.kind === 'quota' && loadPlans()));
  return () => (off(), clearInterval(planTimer));
}

function niceTicks(max) {
  const step = Math.pow(10, Math.floor(Math.log10(max)));
  const s = max / step > 5 ? step * 2 : max / step > 2 ? step : step / 2;
  const out = [];
  for (let v = 0; v <= max * 1.001; v += Math.max(1, s)) out.push(v);
  return out;
}

/** A bar with 4px rounded top corners, square at the baseline. */
function roundTop(x, y, w, hh) {
  const r = Math.min(4, w / 2, hh);
  return `M${x},${y + hh}V${y + r}Q${x},${y} ${x + r},${y}H${x + w - r}Q${x + w},${y} ${x + w},${y + r}V${y + hh}Z`;
}

export { S, ic };
