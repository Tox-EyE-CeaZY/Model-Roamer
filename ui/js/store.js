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
  agents: {}, // id → status
  running: {}, // thread id → {turn, started}
  taskKinds: [],
  dataDir: '',
};

const subs = new Set();
/** fn(what) whenever the store changes ('projects' | 'agents' | 'settings' | 'running' | 'all'). */
export const watch = (fn) => (subs.add(fn), () => subs.delete(fn));
const changed = (what) => subs.forEach((f) => f(what));

export async function load() {
  const st = await get('state');
  S.version = st.version;
  S.server = st.server;
  S.me = st.me;
  S.settings = st.settings;
  S.projects = st.projects;
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
    const st = await get('state');
    S.projects = st.projects;
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
export const project = (id) => S.projects.find((p) => p.id === id);
export const agentList = () => Object.values(S.agents);
