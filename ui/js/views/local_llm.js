// Settings › Local model: on-device llama.cpp + GGUF model manager.
// Talks to GET /api/local-llm/status, GET /api/local-llm/update,
// POST /api/local-llm/{setup,start,stop,update,rollback,cancel}
// and listens to live `local_llm` bus events for download progress & runtime state.

import { h, dur } from '../util.js';
import { get, post, listen } from '../api.js';
import { S } from '../store.js';
import { dialog, toast } from '../ui.js';
import { ic, loading, errorBox, empty, kicker, copyable } from '../common.js';

const MIB = 1024 * 1024;
const GIB = 1024 * MIB;
const ENGINE_EST_BYTES = 38 * 1000 * 1000;

const MODEL_META = {
  'qwen3-4b-instruct-2507': {
    tier: '4 GB+ GPU',
    note: 'Fast 4B instruct model with strict tool calling (Apache 2.0).',
  },
  'smollm3-3b': {
    tier: 'CPU / light',
    note: 'Compact 3B model tuned for CPU-only machines (Apache 2.0).',
  },
  'gemma-4-e2b-it': {
    tier: '4.5 GB+ GPU',
    note: 'Google Edge 2B-effective model; alternative for 4.5 GB+ VRAM (Apache 2.0).',
  },
};

const BACKEND_LABEL = {
  vulkan: 'Vulkan GPU',
  metal: 'Apple Metal',
  cpu: 'CPU',
};

const STAGE_LABEL = {
  setup: 'Setting up local model',
  engine: 'llama.cpp engine',
  model: 'model weights',
  update: 'Updating llama.cpp',
  rollback: 'Rolling back llama.cpp',
};

/** Decimal GB/MB for download sizes so 2,497,280,448 B displays as "~2.5 GB". */
function approxSize(n) {
  const v = Number(n) || 0;
  if (v >= 1e9) return `~${(v / 1e9).toFixed(1)} GB`;
  if (v >= 1e6) return `~${Math.round(v / 1e6)} MB`;
  if (v >= 1e3) return `~${Math.round(v / 1e3)} KB`;
  return `${v} B`;
}

function exactSize(n) {
  const v = Number(n) || 0;
  if (v >= 1e9) return `${(v / 1e9).toFixed(2)} GB`;
  if (v >= 1e6) return `${(v / 1e6).toFixed(1)} MB`;
  if (v >= 1e3) return `${(v / 1e3).toFixed(0)} KB`;
  return `${v} B`;
}

function fmtSpeed(bps) {
  if (!bps || !Number.isFinite(bps) || bps <= 0) return '';
  if (bps >= 1e9) return `${(bps / 1e9).toFixed(2)} GB/s`;
  if (bps >= 1e6) return `${(bps / 1e6).toFixed(1)} MB/s`;
  if (bps >= 1e3) return `${Math.round(bps / 1e3)} KB/s`;
  return `${Math.round(bps)} B/s`;
}

function fmtMib(mib) {
  if (mib == null || Number.isNaN(Number(mib))) return 'Unknown';
  const n = Number(mib);
  if (n >= 1024) return `${(n / 1024).toFixed(1)} GiB`;
  return `${Math.round(n)} MiB`;
}

function fmtContext(ctx) {
  if (!ctx) return 'Auto';
  return `${Number(ctx).toLocaleString()} tokens`;
}

function joinPath(base, leaf) {
  if (!base) return leaf;
  const sep = base.includes('\\') || S.server?.os === 'windows' ? '\\' : '/';
  return base.endsWith(sep) ? base + leaf : base + sep + leaf;
}

function isBusyProgress(p) {
  if (!p) return false;
  return Boolean(p.running || ['starting', 'downloading', 'extracting', 'checking'].includes(p.state));
}

function progressHeadline(p) {
  if (!p) return 'Working…';
  const target = STAGE_LABEL[p.stage] || p.stage || 'local model';
  switch (p.state) {
    case 'downloading': return `Downloading ${target}`;
    case 'extracting': return `Extracting ${target}`;
    case 'checking': return `Verifying ${target}`;
    case 'starting': return p.stage === 'update'
      ? 'Starting engine update'
      : p.stage === 'rollback'
        ? 'Starting engine rollback'
        : 'Preparing local model setup';
    default: return 'Working…';
  }
}

export default async function localModel(el) {
  const root = h('div', { class: 'llm-view' });
  const statusPanel = h('section', { class: 'panel cut', 'aria-label': 'Local model status' });
  const hwPanel = h('section', { class: 'panel cut', 'aria-label': 'Detected hardware' });
  const modelsPanel = h('section', { class: 'panel cut', 'aria-label': 'Model selection' });
  const updatesPanel = h('section', { class: 'panel cut', 'aria-label': 'Engine version and updates' });
  const storagePanel = h('section', { class: 'panel cut', 'aria-label': 'Storage and disk space' });
  root.append(statusPanel, hwPanel, modelsPanel, updatesPanel, storagePanel);
  el.replaceChildren(loading('Checking local model'));

  let alive = true;
  let st = null;
  let selectedId = null;
  let pendingOp = null; // 'start' | 'stop' | 'setup' | 'update' | 'rollback' | 'cancel' | 'check' | null
  let updateInfo = null; // { active_tag, latest_tag, available, error?, checked_at }
  let lastAction = null; // { kind: 'setup' | 'update' | 'rollback' | 'start', model?: string }
  let prevModelBeforeSwitch = null;
  let dismissedErr = '';
  let pollTimer = 0;

  // Rolling speed window for live download progress
  const samples = [];
  let lastStage = '';
  let lastBytes = 0;

  function recordProgressSample(p) {
    if (!p || p.state !== 'downloading') {
      samples.length = 0;
      lastStage = p?.stage || '';
      lastBytes = 0;
      return;
    }
    const now = Date.now();
    const b = Number(p.bytes) || 0;
    if (p.stage !== lastStage || b < lastBytes) {
      samples.length = 0;
    }
    lastStage = p.stage || '';
    lastBytes = b;
    const prev = samples[samples.length - 1];
    if (!prev || b !== prev.bytes || now - prev.t >= 500) {
      samples.push({ t: now, bytes: b });
    }
    while (samples.length > 2 && now - samples[0].t > 8000) samples.shift();
  }

  function currentSpeedBps(p) {
    const explicit = Number(p?.speed_bps ?? p?.bytes_per_sec);
    if (explicit > 0) return explicit;
    if (samples.length < 2) return 0;
    const first = samples[0];
    const last = samples[samples.length - 1];
    const dt = (last.t - first.t) / 1000;
    const db = last.bytes - first.bytes;
    if (dt < 0.35 || db <= 0) return 0;
    return db / dt;
  }

  const findModel = (id) => (st?.models || []).find((m) => m.id === id) || null;
  const activeModel = () => (st?.active?.model ? findModel(st.active.model) : null);
  const recommendedModel = () => (st?.recommended_model ? findModel(st.recommended_model) : null) || (st?.models || [])[0] || null;
  const chosenModel = () => findModel(selectedId) || activeModel() || recommendedModel();

  function modelsDir() {
    return st?.models_dir || st?.disk?.path || joinPath(S.dataDir || '', 'models');
  }

  function diskFreeBytes() {
    const v = st?.disk?.free_bytes ?? st?.free_bytes;
    return typeof v === 'number' && Number.isFinite(v) ? v : null;
  }

  function diskUsedBytes() {
    const v = st?.disk?.used_bytes ?? st?.used_bytes;
    return typeof v === 'number' && Number.isFinite(v) ? v : null;
  }

  function stopPoll() {
    clearInterval(pollTimer);
    pollTimer = 0;
  }

  function ensurePoll() {
    if (isBusyProgress(st?.progress)) {
      if (!pollTimer) {
        pollTimer = setInterval(() => loadStatus({ silent: true }), 3000);
      }
    } else {
      stopPoll();
    }
  }

  async function loadStatus({ silent = false } = {}) {
    try {
      const next = await get('local-llm/status', { quiet: silent });
      if (!alive) return;
      st = next;
      if (!selectedId || !findModel(selectedId)) {
        selectedId = st.active?.model || st.recommended_model || st.models?.[0]?.id || null;
      }
      recordProgressSample(st.progress);
      if (!root.isConnected) el.replaceChildren(root);
      drawAll();
      ensurePoll();
    } catch (e) {
      if (!alive) return;
      if (!st) {
        el.replaceChildren(h('section', { class: 'panel cut' }, kicker('Local model'), errorBox(e, () => {
          el.replaceChildren(loading('Checking local model'));
          loadStatus();
        })));
      } else if (!silent) {
        toast(e.message, { err: true });
      }
    }
  }

  // ------------------------------------------------------------ actions
  async function confirmAndSetup(modelId, { skipConfirm = false } = {}) {
    const target = findModel(modelId) || chosenModel();
    if (!target) return;
    const switching = Boolean(st?.ready && st?.active?.model && st.active.model !== target.id);
    const needsEngine = !st?.active?.tag;
    const downloadBytes = target.bytes + (needsEngine ? ENGINE_EST_BYTES : 0);
    const free = diskFreeBytes();
    const requiredSpace = Math.round(downloadBytes * 1.25 + 500 * MIB);

    if (!skipConfirm) {
      const dlText = approxSize(target.bytes);
      const engineNote = needsEngine ? ` plus the llama.cpp runtime (${approxSize(ENGINE_EST_BYTES)})` : '';
      const facts = h('dl', { class: 'facts llm-dlg-facts' },
        h('dt', {}, 'Model'), h('dd', {}, target.name),
        h('dt', {}, 'Download'), h('dd', { class: 'mono' }, `${dlText} (${exactSize(target.bytes)})${needsEngine ? ` + ${approxSize(ENGINE_EST_BYTES)} engine` : ''}`),
        h('dt', {}, 'Destination'), h('dd', { class: 'mono small' }, modelsDir()),
        free != null ? [
          h('dt', {}, 'Free space'),
          h('dd', { class: 'mono' + (free < requiredSpace ? ' hot' : '') }, `${exactSize(free)} available (${exactSize(requiredSpace)} recommended)`),
        ] : null);
      const ok = await dialog({
        title: switching ? `Switch to ${target.name}?` : `Set up ${target.name}?`,
        text: `This downloads ${dlText}${engineNote} to your computer and checks its SHA-256 hash before enabling it. Partial downloads resume automatically if interrupted.`,
        body: facts,
        ok: `Download ${dlText}`,
        cancel: 'Cancel',
      });
      if (!ok) return;
    }

    dismissedErr = '';
    lastAction = { kind: 'setup', model: target.id };
    prevModelBeforeSwitch = switching ? st.active.model : null;
    pendingOp = 'setup';
    drawStatus();
    drawModels();
    try {
      await post('local-llm/setup?model=' + encodeURIComponent(target.id));
      if (!alive) return;
      st = {
        ...st,
        progress: { state: 'starting', stage: 'setup', running: true, bytes: 0, total: target.bytes, pct: 0, error: null },
      };
      recordProgressSample(st.progress);
      toast(`Started downloading ${target.name} (${approxSize(target.bytes)})`);
      ensurePoll();
    } catch (e) {
      if (!alive) return;
      st = {
        ...st,
        progress: { ...(st?.progress || {}), state: 'error', stage: 'setup', running: false, error: e.message },
      };
      toast(e.message, { err: true, ms: 6000 });
    } finally {
      pendingOp = null;
      if (alive) drawAll();
    }
  }

  async function startServer({ quietToast = false } = {}) {
    dismissedErr = '';
    lastAction = { kind: 'start' };
    pendingOp = 'start';
    if (st) {
      st = { ...st, runtime: { ...(st.runtime || {}), state: 'starting', error: null } };
      drawStatus();
    }
    try {
      const next = await post('local-llm/start', {}, { timeout: 90000 });
      if (!alive) return;
      st = next;
      if (!quietToast) toast('Local model is running');
    } catch (e) {
      if (!alive) return;
      st = { ...st, runtime: { ...(st?.runtime || {}), state: 'error', error: e.message } };
      toast(e.message, { err: true, ms: 7000 });
    } finally {
      pendingOp = null;
      if (alive) drawAll();
    }
  }

  async function stopServer() {
    pendingOp = 'stop';
    drawStatus();
    try {
      const next = await post('local-llm/stop');
      if (!alive) return;
      st = next;
      toast('Local model stopped', {
        action: 'Undo',
        ms: 7000,
        run: () => startServer({ quietToast: false }),
      });
    } catch (e) {
      if (!alive) return;
      toast(e.message, { err: true });
    } finally {
      pendingOp = null;
      if (alive) drawAll();
    }
  }

  async function cancelDownload() {
    pendingOp = 'cancel';
    drawStatus();
    try {
      await post('local-llm/cancel');
      if (!alive) return;
      toast('Download cancelled.');
      await loadStatus({ silent: true });
    } catch (e) {
      if (!alive) return;
      if (e.status === 404 || e.status === 405) {
        toast('This server build does not support cancelling an active download yet.', { err: true, ms: 6000 });
      } else {
        toast(e.message, { err: true, ms: 6000 });
      }
    } finally {
      pendingOp = null;
      if (alive) drawStatus();
    }
  }

  async function checkUpdates() {
    pendingOp = 'check';
    drawUpdates();
    try {
      const r = await get('local-llm/update', { timeout: 25000 });
      if (!alive) return;
      updateInfo = { ...r, checked_at: Date.now() };
      if (r.available) {
        toast(`llama.cpp ${r.latest_tag} is available`);
      } else {
        toast(`llama.cpp is up to date (${r.active_tag || r.latest_tag})`);
      }
    } catch (e) {
      if (!alive) return;
      updateInfo = { error: e.message, checked_at: Date.now() };
      toast(e.message, { err: true });
    } finally {
      pendingOp = null;
      if (alive) drawUpdates();
    }
  }

  async function applyUpdate({ skipConfirm = false } = {}) {
    const latest = updateInfo?.latest_tag;
    if (!skipConfirm) {
      const ok = await dialog({
        title: latest ? `Update llama.cpp to ${latest}?` : 'Update llama.cpp engine?',
        text: `Downloads the newer prebuilt llama.cpp archive (~25–40 MB), verifies its SHA-256 digest, and test-launches it before switching. Your current build (${st?.active?.tag || st?.pinned_tag}) stays installed so you can roll back.`,
        ok: latest ? `Update to ${latest}` : 'Update',
        cancel: 'Cancel',
      });
      if (!ok) return;
    }
    dismissedErr = '';
    lastAction = { kind: 'update' };
    pendingOp = 'update';
    drawStatus();
    drawUpdates();
    try {
      await post('local-llm/update');
      if (!alive) return;
      st = {
        ...st,
        progress: { state: 'starting', stage: 'update', running: true, bytes: 0, total: 0, pct: 0, error: null },
      };
      toast('Updating llama.cpp…');
      ensurePoll();
    } catch (e) {
      if (!alive) return;
      st = { ...st, progress: { ...(st?.progress || {}), state: 'error', stage: 'update', running: false, error: e.message } };
      toast(e.message, { err: true, ms: 6000 });
    } finally {
      pendingOp = null;
      if (alive) drawAll();
    }
  }

  async function applyRollback({ skipConfirm = false } = {}) {
    const prevTag = st?.previous?.tag;
    if (!prevTag) return;
    if (!skipConfirm) {
      const ok = await dialog({
        title: `Roll back to ${prevTag}?`,
        text: `Switches llama.cpp from ${st?.active?.tag || 'current'} back to ${prevTag} and stops the running server so the next turn uses ${prevTag}. You can swap back at any time.`,
        ok: `Roll back to ${prevTag}`,
        cancel: 'Cancel',
      });
      if (!ok) return;
    }
    dismissedErr = '';
    lastAction = { kind: 'rollback' };
    pendingOp = 'rollback';
    drawStatus();
    drawUpdates();
    try {
      await post('local-llm/rollback');
      if (!alive) return;
      st = {
        ...st,
        progress: { state: 'starting', stage: 'rollback', running: true, bytes: 0, total: 0, pct: 0, error: null },
      };
      ensurePoll();
    } catch (e) {
      if (!alive) return;
      st = { ...st, progress: { ...(st?.progress || {}), state: 'error', stage: 'rollback', running: false, error: e.message } };
      toast(e.message, { err: true, ms: 6000 });
    } finally {
      pendingOp = null;
      if (alive) drawAll();
    }
  }

  function retryLastFailure() {
    dismissedErr = '';
    if (lastAction?.kind === 'update' || st?.progress?.stage === 'update') return applyUpdate({ skipConfirm: true });
    if (lastAction?.kind === 'rollback' || st?.progress?.stage === 'rollback') return applyRollback({ skipConfirm: true });
    if (st?.ready && (st?.runtime?.state === 'error' || st?.runtime?.state === 'backoff' || lastAction?.kind === 'start')) {
      return startServer();
    }
    return confirmAndSetup(lastAction?.model || selectedId, { skipConfirm: false });
  }

  // ------------------------------------------------------------ rendering
  function deriveState() {
    const p = st.progress || {};
    const rt = st.runtime || {};
    if (st.supported === false) {
      return {
        tone: 'faint',
        chip: 'Unsupported',
        headline: 'No prebuilt engine for this platform',
        sub: 'Prebuilt llama.cpp packages are available for Windows (x64, ARM64), Linux (x64, ARM64), and macOS.',
      };
    }
    if (isBusyProgress(p)) {
      return {
        tone: 'live',
        chip: p.state === 'downloading' ? `Downloading ${p.pct || 0}%` : p.state === 'extracting' ? 'Extracting' : p.state === 'checking' ? 'Verifying' : 'Working',
        spin: p.state !== 'downloading',
        pulse: p.state === 'downloading',
        headline: progressHeadline(p),
        sub: 'Model Roamer verifies SHA-256 digests before activating downloaded files.',
      };
    }
    const errText = st.error || (p.state === 'error' ? p.error : null);
    if (errText && errText !== dismissedErr && !st.ready) {
      return {
        tone: 'hot',
        chip: 'Error',
        headline: 'Local model setup did not finish',
        sub: 'Partial downloads are kept on disk so retrying picks up where it left off.',
      };
    }
    if (!st.ready) {
      const rec = recommendedModel();
      return {
        tone: 'faint',
        chip: 'Not installed',
        headline: 'Run an assistant on this machine',
        sub: rec
          ? `Recommended for your hardware: ${rec.name} (${approxSize(rec.bytes)}). Free, private, loopback-only, and unloads after ${Math.round((st.idle_seconds || 900) / 60)} min idle.`
          : 'Download a pinned llama.cpp runtime and GGUF model to run locally.',
      };
    }
    const act = activeModel();
    const idleMin = Math.round((st.idle_seconds || 900) / 60);
    if (rt.state === 'running') {
      const bk = BACKEND_LABEL[rt.backend] || rt.backend || BACKEND_LABEL[st.profile?.backend] || 'Local';
      return {
        tone: 'live',
        chip: 'Running',
        pulse: true,
        headline: act?.name || st.active?.model || 'Local model',
        sub: `${bk} · ${fmtContext(rt.context || st.profile?.context)} context · llama.cpp ${st.active?.tag || st.pinned_tag} · unloads after ${idleMin}m idle`,
      };
    }
    if (rt.state === 'starting') {
      return {
        tone: 'dim',
        chip: 'Starting',
        spin: true,
        headline: act?.name || st.active?.model || 'Local model',
        sub: 'Launching loopback llama-server and waiting for its health check…',
      };
    }
    if (rt.state === 'backoff') {
      return {
        tone: 'hot',
        chip: 'Restarting',
        spin: true,
        headline: act?.name || st.active?.model || 'Local model',
        sub: 'The server exited unexpectedly and is backing off before retrying.',
      };
    }
    if (rt.state === 'error') {
      return {
        tone: 'hot',
        chip: 'Error',
        headline: act?.name || st.active?.model || 'Local model',
        sub: 'llama-server stopped with an error. Retry starting it or check hardware warnings below.',
      };
    }
    return {
      tone: 'acc',
      chip: 'Ready · Idle',
      headline: act?.name || st.active?.model || 'Local model',
      sub: `Installed (${st.active?.tag || st.pinned_tag}) and idle. Starts automatically when needed, or start it now to warm it up.`,
    };
  }

  function renderProgressBay(p) {
    const isDl = p.state === 'downloading' && Number(p.total) > 0;
    const pct = isDl ? Math.max(0, Math.min(100, Number(p.pct) || Math.round((p.bytes / p.total) * 100))) : 0;
    const remaining = isDl ? Math.max(0, Number(p.total) - Number(p.bytes)) : 0;
    const speed = isDl ? currentSpeedBps(p) : 0;
    const etaSec = speed > 0 && remaining > 0 ? remaining / speed : 0;

    const stats = [];
    if (isDl) {
      stats.push(h('span', {}, `${exactSize(p.bytes)} / ${exactSize(p.total)}`));
      stats.push(h('span', { class: 'llm-prog-sep', 'aria-hidden': 'true' }, '·'));
      stats.push(h('span', {}, `${exactSize(remaining)} remaining`));
      if (speed > 0) {
        stats.push(h('span', { class: 'llm-prog-sep', 'aria-hidden': 'true' }, '·'));
        stats.push(h('span', {}, fmtSpeed(speed)));
        if (etaSec > 0) {
          stats.push(h('span', { class: 'llm-prog-sep', 'aria-hidden': 'true' }, '·'));
          stats.push(h('span', {}, etaSec < 5 ? '< 5s left' : `~${dur(etaSec * 1000)} left`));
        }
      } else {
        stats.push(h('span', { class: 'llm-prog-sep', 'aria-hidden': 'true' }, '·'));
        stats.push(h('span', { class: 'faint' }, 'Measuring speed…'));
      }
    } else {
      stats.push(h('span', {}, p.state === 'extracting' ? 'Unpacking archive and testing llama-server --version…' : p.state === 'checking' ? 'Checking file size and SHA-256 digest…' : 'Preparing download and checking free disk space…'));
    }

    return h('div', { class: 'llm-prog', role: 'status', 'aria-live': 'polite' },
      h('div', { class: 'llm-prog-head' },
        h('span', { class: 'llm-prog-stage' },
          h('span', { class: 'spin', 'aria-hidden': 'true' }),
          progressHeadline(p)),
        h('span', { class: 'llm-prog-pct' }, isDl ? `${pct}%` : p.state.toUpperCase())),
      h('div', {
        class: 'llm-bar' + (isDl ? '' : ' indet'),
        role: 'progressbar',
        'aria-label': progressHeadline(p),
        'aria-valuemin': '0',
        'aria-valuemax': '100',
        'aria-valuenow': isDl ? String(pct) : undefined,
      }, h('div', { class: 'llm-bar-fill', style: isDl ? { width: `${pct}%` } : null })),
      h('div', { class: 'llm-prog-meta' },
        h('div', { class: 'llm-prog-stats' }, stats),
        h('button', {
          class: 'btn xs danger',
          type: 'button',
          disabled: pendingOp === 'cancel',
          onclick: cancelDownload,
        }, ic('close', 12), pendingOp === 'cancel' ? 'Cancelling…' : 'Cancel download')));
  }

  function drawStatus() {
    if (!st) return;
    const info = deriveState();
    const p = st.progress || {};
    const rt = st.runtime || {};
    const busy = isBusyProgress(p);
    const chosen = chosenModel();

    const chip = h('span', { class: 'status ' + info.tone },
      info.pulse ? h('i', { class: 'pulse', 'aria-hidden': 'true' }) : info.spin ? h('span', { class: 'spin', 'aria-hidden': 'true' }) : null,
      info.chip);

    // Error & warning callouts
    const errMsg = (p.state === 'error' && p.error) || st.error || (rt.state === 'error' || rt.state === 'backoff' ? rt.error : null);
    const showErr = errMsg && errMsg !== dismissedErr && !busy;
    const warnings = [st.warning, rt.warning].filter(Boolean);

    // Primary action buttons
    const acts = [];
    if (st.supported !== false && !busy) {
      if (!st.ready) {
        if (chosen) {
          acts.push(h('button', {
            class: showErr ? 'btn' : 'btn primary',
            type: 'button',
            disabled: pendingOp === 'setup',
            onclick: () => confirmAndSetup(chosen.id),
          }, ic('download', 14), `Set up local model (${approxSize(chosen.bytes)})`));
        }
      } else {
        const running = rt.state === 'running';
        const starting = rt.state === 'starting' || pendingOp === 'start';
        const backoff = rt.state === 'backoff';
        if (running) {
          acts.push(h('button', {
            class: 'btn danger',
            type: 'button',
            disabled: pendingOp === 'stop',
            onclick: stopServer,
          }, ic('stop', 13), pendingOp === 'stop' ? 'Stopping…' : 'Stop'));
        } else {
          acts.push(h('button', {
            class: showErr ? 'btn' : 'btn primary',
            type: 'button',
            disabled: starting || pendingOp === 'stop',
            onclick: () => startServer(),
          }, ic('play', 13), starting ? 'Starting…' : rt.state === 'error' ? 'Retry start' : 'Start'));
          if (backoff) {
            acts.push(h('button', {
              class: 'btn sm danger',
              type: 'button',
              disabled: pendingOp === 'stop',
              onclick: stopServer,
            }, ic('stop', 12), 'Stop retries'));
          }
        }
      }
      acts.push(h('button', {
        class: 'btn sm',
        type: 'button',
        title: 'Refresh local model status',
        onclick: () => loadStatus(),
      }, ic('refresh', 13), 'Refresh'));
    }

    // Agent picker readiness note
    const localAgent = S.agents?.local;
    const agentNote = st.ready
      ? localAgent
        ? 'Installed — "Local model" is available in thread agent pickers.'
        : 'Installed and ready. Once the local agent adapter is active on this build, "Local model" appears in thread agent pickers automatically.'
      : 'Once installed, "Local model" appears in thread agent pickers (hidden while not installed).';

    statusPanel.replaceChildren(
      kicker('Local model', chip),
      h('div', { class: 'llm-hero tone-' + info.tone },
        h('div', { class: 'llm-hero-main' },
          h('div', { class: 'llm-hero-title' },
            h('h2', {}, info.headline),
            st.ready && st.active?.tag ? h('span', { class: 'tag acc' }, st.active.tag) : null),
          h('p', { class: 'llm-hero-sub' }, info.sub),
          acts.length ? h('div', { class: 'llm-hero-acts' }, acts) : null)),
      busy ? renderProgressBay(p) : null,
      showErr ? h('div', { class: 'llm-alert', role: 'alert' },
        ic('warn', 17),
        h('div', { class: 'llm-alert-body' },
          h('b', {}, p.state === 'error' ? `${STAGE_LABEL[p.stage] || 'Setup'} failed` : st.error ? 'Installation manifest error' : 'Runtime error'),
          h('p', {}, errMsg)),
        h('div', { class: 'llm-alert-acts' },
          h('button', { class: 'btn sm primary', type: 'button', onclick: retryLastFailure }, ic('refresh', 12), 'Retry'),
          h('button', { class: 'btn xs', type: 'button', onclick: () => { dismissedErr = errMsg; drawStatus(); } }, 'Dismiss'))) : null,
      ...warnings.map((w) => h('div', { class: 'llm-warn', role: 'status' }, ic('warn', 15), h('span', {}, w))),
      h('div', { class: 'llm-note-strip' }, ic('cpu', 13), h('span', {}, agentNote)));
  }

  function drawHardware() {
    if (!st) return;
    const hw = st.hardware || {};
    const gpu = hw.gpu;
    const prof = st.profile || {};
    const rec = recommendedModel();

    const gpuTitle = hw.metal
      ? 'Apple Silicon (Metal)'
      : gpu?.name || 'CPU only';
    const gpuSub = gpu
      ? `${fmtMib(gpu.vram_mib)} VRAM${gpu.device ? ' · ' + gpu.device : ''}`
      : 'No 4+ GiB Vulkan/Metal GPU detected';

    const ramTitle = fmtMib(hw.ram_mib);
    const ramSub = hw.ram_mib && hw.ram_mib < 6144 ? 'Below 6 GiB CPU threshold' : 'System memory';

    const tierBackend = BACKEND_LABEL[prof.backend] || prof.backend || 'CPU';
    const tierTitle = `${tierBackend} · ${fmtContext(prof.context)}`;
    const tierSub = rec ? `Recommended: ${rec.name} (${approxSize(rec.bytes)})` : 'Auto-selected profile';

    hwPanel.replaceChildren(
      kicker('Detected hardware', h('span', { class: 'tag' + (prof.backend && prof.backend !== 'cpu' ? ' acc' : '') }, tierBackend)),
      h('div', { class: 'llm-hw-grid' },
        h('div', { class: 'llm-stat' },
          h('span', { class: 'llm-stat-k' }, 'Accelerator'),
          h('span', { class: 'llm-stat-v' }, gpuTitle),
          h('span', { class: 'llm-stat-s' }, gpuSub)),
        h('div', { class: 'llm-stat' },
          h('span', { class: 'llm-stat-k' }, 'Memory'),
          h('span', { class: 'llm-stat-v' }, ramTitle),
          h('span', { class: 'llm-stat-s' }, ramSub)),
        h('div', { class: 'llm-stat' },
          h('span', { class: 'llm-stat-k' }, 'Recommended tier'),
          h('span', { class: 'llm-stat-v' }, tierTitle),
          h('span', { class: 'llm-stat-s' }, tierSub))));
  }

  function drawModels() {
    if (!st) return;
    const list = st.models || [];
    const busy = isBusyProgress(st.progress);
    const actId = st.ready ? st.active?.model : null;
    const recId = st.recommended_model;

    if (!list.length) {
      modelsPanel.replaceChildren(
        kicker('Models'),
        empty('cpu', 'No local models listed', 'The engine did not advertise any pinned GGUF models.'));
      return;
    }

    const group = h('div', { class: 'llm-models', role: 'radiogroup', 'aria-label': 'Choose a local model' });
    const btns = list.map((m, idx) => {
      const meta = MODEL_META[m.id] || {};
      const isSelected = m.id === selectedId;
      const isActive = m.id === actId;
      const isRec = m.id === recId;
      const btn = h('button', {
        class: 'llm-model cut' + (isSelected ? ' selected' : '') + (isActive ? ' active' : ''),
        type: 'button',
        role: 'radio',
        'aria-checked': String(isSelected),
        tabindex: isSelected ? '0' : '-1',
        disabled: busy || st.supported === false,
        onclick: () => {
          selectedId = m.id;
          drawAll();
        },
        onkeydown: (e) => {
          if (e.key === 'ArrowDown' || e.key === 'ArrowRight') {
            e.preventDefault();
            const next = list[(idx + 1) % list.length];
            selectedId = next.id;
            drawAll();
            modelsPanel.querySelectorAll('[role="radio"]')[(idx + 1) % list.length]?.focus();
          } else if (e.key === 'ArrowUp' || e.key === 'ArrowLeft') {
            e.preventDefault();
            const prevIdx = (idx - 1 + list.length) % list.length;
            selectedId = list[prevIdx].id;
            drawAll();
            modelsPanel.querySelectorAll('[role="radio"]')[prevIdx]?.focus();
          }
        },
      },
      h('span', { class: 'llm-radio', 'aria-hidden': 'true' }),
      h('div', { class: 'llm-m-info' },
        h('div', { class: 'llm-m-head' },
          h('span', { class: 'llm-m-name' }, m.name),
          isActive ? h('span', { class: 'tag acc' }, 'Active') : null,
          isRec ? h('span', { class: 'tag' }, 'Recommended') : null,
          meta.tier ? h('span', { class: 'tag' }, meta.tier) : null),
        meta.note ? h('span', { class: 'llm-m-note' }, meta.note) : null,
        h('span', { class: 'llm-m-id' }, m.id)),
      h('span', { class: 'llm-m-size' }, approxSize(m.bytes)));
      return btn;
    });
    group.append(...btns);

    const chosen = chosenModel();
    const needsSwitch = Boolean(chosen && st.ready && chosen.id !== actId);
    const needsInitial = Boolean(chosen && !st.ready && st.supported !== false);

    modelsPanel.replaceChildren(
      kicker('Model choice', h('small', { class: 'faint' }, 'one resident model at a time · pinned SHA-256')),
      group,
      !busy && needsSwitch ? h('div', { class: 'llm-stage-bar' },
        h('p', {}, `Ready to switch from ${activeModel()?.name || actId} to `, h('b', {}, chosen.name), ` (${approxSize(chosen.bytes)} download if not already cached).`),
        h('div', { class: 'row' },
          h('button', {
            class: 'btn sm',
            type: 'button',
            onclick: () => { selectedId = actId; drawAll(); },
          }, 'Keep current'),
          h('button', {
            class: 'btn sm primary',
            type: 'button',
            onclick: () => confirmAndSetup(chosen.id),
          }, ic('download', 13), `Switch & download (${approxSize(chosen.bytes)})`))) : null,
      !busy && needsInitial && chosen.id !== recId ? h('div', { class: 'llm-stage-bar' },
        h('p', {}, 'Selected ', h('b', {}, chosen.name), ` (${approxSize(chosen.bytes)}) instead of the hardware-recommended model.`),
        h('button', {
          class: 'btn xs',
          type: 'button',
          onclick: () => { selectedId = recId; drawAll(); },
        }, 'Use recommended')) : null);
  }

  function drawUpdates() {
    if (!st) return;
    const busy = isBusyProgress(st.progress);
    const activeTag = st.active?.tag || null;
    const prevTag = st.previous?.tag || null;
    const pinnedTag = st.pinned_tag || '—';

    let updNode = null;
    if (updateInfo?.error) {
      updNode = h('div', { class: 'llm-upd-box err', role: 'alert' },
        h('span', {}, `Update check failed: ${updateInfo.error}`),
        h('button', { class: 'btn xs', type: 'button', disabled: pendingOp === 'check', onclick: checkUpdates }, 'Retry'));
    } else if (updateInfo?.available) {
      updNode = h('div', { class: 'llm-upd-box avail', role: 'status' },
        h('span', {}, h('b', {}, `llama.cpp ${updateInfo.latest_tag}`), ` is available (installed: ${updateInfo.active_tag || activeTag}).`),
        h('button', {
          class: 'btn sm primary',
          type: 'button',
          disabled: busy || pendingOp === 'update',
          onclick: () => applyUpdate(),
        }, ic('download', 13), `Update to ${updateInfo.latest_tag}`));
    } else if (updateInfo) {
      updNode = h('div', { class: 'llm-upd-box', role: 'status' },
        h('span', { class: 'dim' }, activeTag
          ? `Up to date on ${activeTag} (latest compatible build: ${updateInfo.latest_tag}).`
          : `Latest compatible llama.cpp build is ${updateInfo.latest_tag} (pinned baseline: ${pinnedTag}).`));
    }

    updatesPanel.replaceChildren(
      kicker('Engine & updates', h('span', { class: 'tag' }, 'llama.cpp')),
      h('dl', { class: 'facts llm-facts' },
        h('dt', {}, 'Pinned version'), h('dd', { class: 'mono' }, pinnedTag),
        h('dt', {}, 'Installed build'), h('dd', { class: 'mono' }, activeTag || 'Not installed'),
        h('dt', {}, 'Rollback build'), h('dd', { class: 'mono' }, prevTag ? `${prevTag}${st.previous?.model ? ' · ' + st.previous.model : ''}` : 'None yet')),
      updNode,
      h('div', { class: 'row' },
        h('button', {
          class: 'btn sm',
          type: 'button',
          disabled: busy || pendingOp === 'check' || st.supported === false,
          onclick: checkUpdates,
        }, pendingOp === 'check' ? h('span', { class: 'spin', 'aria-hidden': 'true' }) : ic('refresh', 13), pendingOp === 'check' ? 'Checking…' : 'Check for updates'),
        prevTag ? h('button', {
          class: 'btn sm',
          type: 'button',
          disabled: busy || pendingOp === 'rollback',
          onclick: () => applyRollback(),
        }, ic('undo', 13), `Roll back to ${prevTag}`) : null));
  }

  function drawStorage() {
    if (!st) return;
    const dir = modelsDir();
    const free = diskFreeBytes();
    const used = diskUsedBytes();
    const chosen = chosenModel();
    const act = activeModel();
    const needed = chosen ? Math.round(chosen.bytes * 1.25 + 500 * MIB) : 0;
    const lowSpace = free != null && needed > 0 && free < needed;

    let diskRow;
    if (free != null) {
      const pct = needed > 0 ? Math.min(100, Math.round((needed / Math.max(free, needed)) * 100)) : 0;
      diskRow = h('div', { class: 'llm-disk-bar' },
        h('span', { class: 'mono' + (lowSpace ? ' hot' : '') },
          `${exactSize(free)} free`,
          used != null ? ` · ${exactSize(used)} used by local models` : '',
          lowSpace ? ` · needs ${exactSize(needed)}` : ''),
        !st.ready && needed > 0 ? h('div', { class: 'llm-disk-track', title: `${exactSize(needed)} recommended free space` },
          h('div', { class: 'llm-disk-fill' + (lowSpace ? ' low' : ''), style: { width: `${pct}%` } })) : null);
    } else {
      diskRow = h('span', { class: 'dim' },
        st.ready && act
          ? `${exactSize(act.bytes)} active model weights (+ ~35–70 MB llama.cpp engine). Free space is verified before each download (1.25× + 500 MB headroom).`
          : chosen
            ? `Needs ${exactSize(needed)} free space for ${chosen.name} (1.25× download + 500 MB headroom, checked before download starts).`
            : 'Checked automatically before download (1.25× + 500 MB headroom).');
    }

    storagePanel.replaceChildren(
      kicker('Storage'),
      h('dl', { class: 'facts llm-facts' },
        h('dt', {}, 'Models folder'), h('dd', {}, copyable(dir)),
        h('dt', {}, 'Disk space'), h('dd', {}, diskRow),
        h('dt', {}, 'Layout'), h('dd', { class: 'mono small faint' }, 'bin/<tag>/ (engine) · weights/ (GGUF) · current.json')),
      h('div', { class: 'row' },
        S.me?.local ? h('button', {
          class: 'btn sm',
          type: 'button',
          onclick: () => post('fs/reveal', { path: dir }).catch((e) => toast(e.message, { err: true })),
        }, ic('folder', 13), 'Show the folder') : null));
  }

  function drawAll() {
    drawStatus();
    drawHardware();
    drawModels();
    drawUpdates();
    drawStorage();
  }

  // ------------------------------------------------------------ live bus events
  const un = listen((ev) => {
    if (!alive) return;
    if (ev.kind === 'resync') {
      loadStatus({ silent: true });
      return;
    }
    if (ev.kind !== 'local_llm' || !ev.data || !st) return;

    if (ev.data.progress) {
      const prevRunning = isBusyProgress(st.progress);
      const nextP = ev.data.progress;
      st = { ...st, progress: nextP };
      recordProgressSample(nextP);

      const nowRunning = isBusyProgress(nextP);
      if (prevRunning && !nowRunning) {
        stopPoll();
        if (nextP.state === 'ready') {
          const completedStage = nextP.stage;
          const undoModel = prevModelBeforeSwitch;
          prevModelBeforeSwitch = null;
          loadStatus({ silent: true }).then(() => {
            if (!alive) return;
            if (completedStage === 'rollback') {
              const curTag = st?.active?.tag || 'previous build';
              toast(`Rolled back to ${curTag}`, {
                action: 'Undo',
                ms: 9000,
                run: () => applyRollback({ skipConfirm: true }),
              });
            } else if (completedStage === 'update') {
              updateInfo = null;
              toast(`Updated llama.cpp to ${st?.active?.tag || 'latest build'}`);
            } else if (undoModel && undoModel !== st?.active?.model) {
              toast(`Switched local model to ${activeModel()?.name || st?.active?.model}`, {
                action: 'Undo',
                ms: 9000,
                run: () => confirmAndSetup(undoModel, { skipConfirm: true }),
              });
            } else {
              toast(`Local model ready: ${activeModel()?.name || 'installed'}`);
            }
          });
          return;
        }
        if (nextP.state === 'error') {
          toast(nextP.error || 'Local model operation failed', { err: true, ms: 7000 });
          loadStatus({ silent: true });
          return;
        }
      }
      drawStatus();
      ensurePoll();
    }

    if (ev.data.runtime) {
      st = { ...st, runtime: ev.data.runtime };
      drawStatus();
    }
  });

  await loadStatus();
  return () => {
    alive = false;
    stopPoll();
    un();
  };
}
