// The app-wide picture: settings, projects, agents, what is running. Kept fresh from the live events.

import { get, patch, listen } from './api.js';
import { debounce, byId } from './util.js';

export const S = {
  ready: false,
  version: '',
  server: {},
  me: {},
  settings: {},
  projects: [],
  collabLinks: [],
  agents: {}, // id → status
  running: {}, // thread id → {turn, started}
  taskKinds: [],
  dataDir: '',
};

// Threads the person pointed at for a chat's next message ("Reference in the lead chat"): chat id → [thread info]
const refsPending = {};
const refSubs = new Set();
export const watchRefs = (fn) => (refSubs.add(fn), () => refSubs.delete(fn));
export function addRef(chatId, t) {
  const info = { id: t.id, title: t.title, agent: t.agent, status: t.status, note: (t.status_note || t.last_body || '').replace(/[#*_`>]/g, '').slice(0, 400) };
  (refsPending[chatId] ||= []).push(info);
  refSubs.forEach((f) => f(chatId));
}
/** Take (and clear) what's waiting for this chat. */
export const takeRefs = (chatId) => refsPending[chatId]?.splice(0) || [];

const subs = new Set();
/** fn(what) whenever the store changes ('projects' | 'agents' | 'settings' | 'running' | 'all'). */
export const watch = (fn) => (subs.add(fn), () => subs.delete(fn));
const changed = (what) => subs.forEach((f) => f(what));

export async function load() {
  const [st, links] = await Promise.all([
    get('state'),
    get('collab/links', { quiet: true }).catch(() => []),
  ]);
  S.version = st.version;
  S.server = st.server;
  S.me = st.me;
  S.settings = st.settings;
  S.projects = st.projects;
  S.collabLinks = Array.isArray(links) ? links : [];
  S.agents = byId(st.agents);
  S.running = Object.fromEntries(st.running.map((r) => [r.thread, r]));
  S.taskKinds = st.task_kinds;
  S.dataDir = st.data_dir;
  S.ready = true;
  applyLook();
  changed('all');
}

const reloadProjects = debounce(async () => {
  try {
    const [st, links] = await Promise.all([
      get('state'),
      get('collab/links', { quiet: true }).catch(() => S.collabLinks),
    ]);
    S.projects = st.projects;
    S.collabLinks = Array.isArray(links) ? links : [];
    S.running = Object.fromEntries(st.running.map((r) => [r.thread, r]));
    changed('projects');
    changed('running');
  } catch { /* the socket will bring us back */ }
}, 350);

listen((ev) => {
  switch (ev.kind) {
    case 'resync':
      load().catch(() => {});
      break;
    case 'project':
    case 'thread':
    case 'decision':
    case 'message':
    case 'collab-links':
    case 'conflict-notes':
      reloadProjects();
      break;
    case 'running':
      if (ev.data?.running === false) delete S.running[ev.thread];
      reloadProjects();
      break;
    case 'turn':
      if (ev.data?.state === 'start') S.running[ev.thread] = { thread: ev.thread, turn: ev.data.turn_id, started: ev.at };
      changed('running');
      break;
    case 'agent':
      // a status event is the whole status: replacing it drops the "pending" of the placeholder it answers
      if (ev.data?.id) S.agents[ev.data.id] = { ...ev.data, pending: false };
      changed('agents');
      break;
    case 'limits':
      get('agents').then((list) => ((S.agents = byId(list)), changed('agents'))).catch(() => {});
      break;
    case 'settings':
      get('settings').then((s) => ((S.settings = s), applyLook(), changed('settings'))).catch(() => {});
      break;
  }
});

/** Change settings: applied here at once, saved right after. */
export async function setSetting(p) {
  const merge = (a, b) => {
    for (const [k, v] of Object.entries(b)) {
      if (v && typeof v === 'object' && !Array.isArray(v) && a[k] && typeof a[k] === 'object') merge(a[k], v);
      else a[k] = v;
    }
  };
  merge(S.settings, p);
  applyLook();
  changed('settings');
  S.settings = await patch('settings', p);
  return S.settings;
}

export function applyLook() {
  const s = S.settings, r = document.documentElement;
  r.dataset.theme = s.theme || 'night';
  r.style.setProperty('--hue', s.hue ?? 128);
  r.classList.toggle('no-topo', s.topo === false);
  r.classList.toggle('no-grain', s.grain === false);
  r.classList.toggle('no-motion', s.motion === false);
  r.dataset.density = s.density || 'cozy';
  try {
    localStorage.setItem('mr-look', JSON.stringify({ theme: s.theme, hue: s.hue }));
  } catch { /* private window */ }
}

/** The agent and model a new chat starts on: the last one picked while the app has been open, else the primary
 * (lead) model from Settings. Closing and reopening the app goes back to the primary. */
export function chatPick() {
  try {
    const p = JSON.parse(sessionStorage.getItem('mr-chat-pick') || 'null');
    if (p?.agent && S.agents[p.agent]) return p;
  } catch { /* private window */ }
  const a = S.settings.default_lead || 'claude';
  return { agent: a, model: S.settings.default_lead_model ?? S.agents[a]?.config?.model ?? '' };
}
export function rememberChatPick(agent, model, effort = '') {
  try {
    sessionStorage.setItem('mr-chat-pick', JSON.stringify({ agent, model, effort }));
  } catch { /* private window */ }
}

export const agent = (id) => S.agents[id] || { id, info: { id, name: id, hue: 200, models: [] }, config: {} };
export const agentName = (id) => agent(id).info?.name || id;
export const agentHue = (id) => agent(id).info?.hue ?? 200;
export const collabLink = (id) => S.collabLinks.find((l) => l.id === id) || null;
export const isJoinedProject = (id) => String(id || '').startsWith('lnk_') || !!collabLink(id);
export const project = (id) => {
  const p = S.projects.find((x) => x.id === id);
  const link = collabLink(id);
  if (p) return link ? { ...p, _collab: link } : p;
  if (link) return { id: link.id, name: link.project_name, color: 128, updated_at: link.last_sync || link.created_at, working: 0, open_threads: 0, unread: 0, decisions: 0, _collab: link };
  return null;
};
/** Every project in the sidebar and Home: own projects plus joined projects (never duplicated). */
export const allProjects = () => {
  const byLink = new Map(S.collabLinks.map((l) => [l.id, l]));
  const seen = new Set();
  const list = [];
  for (const p of S.projects) {
    seen.add(p.id);
    const link = byLink.get(p.id) || null;
    // Skip orphaned local lnk_ rows if the link was already left/deleted
    if (p.id.startsWith('lnk_') && !link) continue;
    list.push(link ? { ...p, _collab: link } : p);
  }
  for (const l of S.collabLinks) {
    if (seen.has(l.id)) continue;
    list.push({
      id: l.id,
      name: l.project_name,
      color: 128,
      updated_at: l.last_sync || l.created_at,
      working: 0,
      open_threads: 0,
      unread: 0,
      decisions: 0,
      _collab: l,
    });
  }
  return list;
};
export const agentList = () => Object.values(S.agents);
