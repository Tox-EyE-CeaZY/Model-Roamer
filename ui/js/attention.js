// What truly waits on the person, decided in one place. Home, the project board, the side rail and the status bar
// all ask here, so they always agree.
//
// Needs the person: an open question (decision), a thread that is blocked, failed or paused on a usage limit, or a
// lead reply they haven't read. Not the person: a worker that finished (`review`). It reports to the lead, which
// reviews it, so it sits in a quiet "Finished" group instead.
//
// The person can mark a thread seen. That holds until the thread changes again (its updated_at moves on), and is
// kept per device.

import { S } from './store.js';

const KEY = 'mr-seen';
let seen = {};
try {
  seen = JSON.parse(localStorage.getItem(KEY) || '{}') || {};
} catch { /* fine: nothing marked seen */ }

const subs = new Set();
/** fn() whenever what needs the person may have changed (marked seen, or the status bar fetched new activity). */
export const watchAttention = (fn) => (subs.add(fn), () => subs.delete(fn));
const changed = () => subs.forEach((f) => f());

const running = (t) => !!(t.running || S.running[t.id]);

/** Why this thread needs the person ('blocked' | 'failed' | 'limited' | 'unread'), or null if it doesn't. */
export function needsYou(t) {
  if (!t || t.resolved || running(t)) return null;
  if (['blocked', 'failed', 'limited'].includes(t.status)) return t.status;
  if (t.kind === 'lead' && t.unread) return 'unread';
  return null;
}

/** A worker that finished its task and reports to the lead: worth seeing, never an alert. */
export const isFinished = (t) => !!t && !t.resolved && !running(t) && t.status === 'review';

/** Marked seen since it last changed. */
export const isSeen = (t) => !!seen[t.id] && seen[t.id] >= (t.updated_at || 0);

function save() {
  // keep the list small: drop marks older than two weeks
  const old = Date.now() - 14 * 864e5;
  for (const [k, v] of Object.entries(seen)) if (v < old) delete seen[k];
  try {
    localStorage.setItem(KEY, JSON.stringify(seen));
  } catch { /* fine: it lasts until reload */ }
  changed();
}
export function markSeen(list) {
  for (const t of [].concat(list)) seen[t.id] = Math.max(t.updated_at || 0, Date.now());
  save();
}
export function unmarkSeen(list) {
  for (const t of [].concat(list)) delete seen[t.id];
  save();
}

/**
 * Sort threads (and open decisions) into what needs the person and what quietly finished.
 * decisions: either a number (a count, as projects carry) or a list. Seen threads drop out of both groups.
 */
export function attention(threads = [], decisions = 0) {
  const fresh = threads.filter((t) => !isSeen(t));
  const need = fresh.filter((t) => needsYou(t));
  const finished = fresh.filter(isFinished);
  const nDecisions = typeof decisions === 'number' ? decisions : decisions.length;
  return { need, finished, decisions: nDecisions, count: nDecisions + need.length };
}

// The latest app-wide picture (the status bar fetches it), so the side rail can show the same count.
let last = { threads: [], decisions: [] };
export function setActivity(a) {
  last = { threads: a.threads || [], decisions: a.decisions || [] };
  changed();
}
/** How many things wait on the person right now, app-wide. */
export const attentionCount = () => attention(last.threads, last.decisions).count;
