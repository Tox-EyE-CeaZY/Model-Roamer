// Voice input for chat composers: live speech-to-text into the textarea via:
//   1) Android native SpeechRecognizer bridge (when running in Model Roamer for Android)
//   2) Browser Web Speech API (Chrome, Edge, Safari)
//   3) Automatic free offline fallback (getUserMedia 16 kHz WAV -> local whisper.cpp on the server)
//      when Web Speech reports network/service-not-allowed (e.g. Windows desktop WebView2) or is absent.
// Supports toggle and push-to-talk modes, cancel (restoring prior text), keyboard shortcut (Ctrl+Shift+M),
// optional first-use model download with progress, clear errors, and persisted settings.

import { h } from './util.js';
import { icon } from './icons.js';
import { api, get, post, listen } from './api.js';
import { S, watch, setSetting } from './store.js';
import { isDesktop, plat } from './native.js';
import { toast, showMenu, overlaid } from './ui.js';
import { ic } from './common.js';

const STORAGE_KEY = 'mr-voice';
const DEFAULTS = { mode: 'toggle', lang: '', auto_send: false };

const LANG_CHOICES = [
  ['', 'Browser default'],
  ['en-US', 'English (US)'],
  ['en-GB', 'English (UK)'],
  ['es-ES', 'Spanish'],
  ['fr-FR', 'French'],
  ['de-DE', 'German'],
  ['it-IT', 'Italian'],
  ['pt-BR', 'Portuguese (BR)'],
  ['ja-JP', 'Japanese'],
  ['ko-KR', 'Korean'],
  ['zh-CN', 'Chinese (Mandarin)'],
];

/** Read saved voice settings from the synced store, falling back to localStorage. */
export function voiceSettings() {
  let local = {};
  try {
    local = JSON.parse(localStorage.getItem(STORAGE_KEY) || '{}') || {};
  } catch { /* private window */ }
  const remote = S.settings?.voice && typeof S.settings.voice === 'object' ? S.settings.voice : {};
  const merged = { ...DEFAULTS, ...local, ...remote };
  return {
    mode: merged.mode === 'ptt' ? 'ptt' : 'toggle',
    lang: typeof merged.lang === 'string' ? merged.lang : '',
    auto_send: !!merged.auto_send,
  };
}

/** Persist a partial voice setting update to both localStorage and Model Roamer settings. */
export async function saveVoiceSettings(patchObj) {
  const next = { ...voiceSettings(), ...patchObj };
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
  } catch { /* private window */ }
  try {
    await setSetting({ voice: next });
  } catch (e) {
    toast(e.message || "Couldn't save voice setting", { err: true });
  }
  return next;
}

// Remember if Web Speech failed with network/service-not-allowed in this session (e.g. WebView2)
let webSpeechBlocked = false;
export function resetVoiceStateForTest() {
  webSpeechBlocked = false;
}

function hasAndroidVoice() {
  if (typeof window === 'undefined') return false;
  if (window.RoamerAndroid && typeof window.RoamerAndroid.startVoice === 'function') {
    try {
      return !window.RoamerAndroid.hasVoice || !!window.RoamerAndroid.hasVoice();
    } catch {
      return false;
    }
  }
  return false;
}

function hasWebSpeech() {
  return typeof window !== 'undefined' && Boolean(window.SpeechRecognition || window.webkitSpeechRecognition);
}

function hasGetUserMedia() {
  return typeof navigator !== 'undefined' && typeof navigator.mediaDevices?.getUserMedia === 'function';
}

/** Available speech recognition backend: 'android' | 'webspeech' | 'local' | null. */
export function voiceBackend() {
  if (typeof window === 'undefined') return null;
  if (hasAndroidVoice()) return 'android';
  if (hasWebSpeech() && !webSpeechBlocked) return 'webspeech';
  if (hasGetUserMedia()) return 'local';
  return null;
}

export function unsupportedReason() {
  if (typeof window !== 'undefined' && window.isSecureContext === false) {
    return 'Voice input needs HTTPS or localhost. Open Model Roamer on this computer or over HTTPS (such as Tailscale HTTPS).';
  }
  return 'This browser does not support Web Speech or microphone recording. Try Chrome, Edge, or Safari.';
}

/** Merge spoken text cleanly into the textarea around a saved selection range. */
export function spliceTranscript(before, after, spoken) {
  const clean = spoken.trim();
  if (!clean) return { text: before + after, caret: before.length };
  const lead = before && !/[\s(\[{'"`]$/.test(before) ? ' ' : '';
  const trail = after && !/^[\s.,!?;:)\]}'"`]/.test(after) ? ' ' : '';
  const left = before + lead + clean;
  return { text: left + trail + after, caret: left.length };
}

/**
 * Downsample Float32 PCM chunks to 16 kHz mono 16-bit PCM WAV bytes for whisper.cpp.
 * Pure JS — requires no FFmpeg or native encoder.
 */
export function encodeWav(chunks, inputSampleRate = 48000, targetRate = 16000) {
  let totalLen = 0;
  for (const c of chunks) totalLen += c.length;
  const merged = new Float32Array(totalLen);
  let off = 0;
  for (const c of chunks) {
    merged.set(c, off);
    off += c.length;
  }

  let samples = merged;
  if (inputSampleRate > 0 && inputSampleRate !== targetRate && merged.length > 0) {
    const ratio = inputSampleRate / targetRate;
    const outLen = Math.max(1, Math.round(merged.length / ratio));
    samples = new Float32Array(outLen);
    for (let i = 0; i < outLen; i++) {
      const start = Math.floor(i * ratio);
      const end = Math.min(merged.length, Math.max(start + 1, Math.floor((i + 1) * ratio)));
      let sum = 0;
      for (let j = start; j < end; j++) sum += merged[j];
      samples[i] = sum / (end - start);
    }
  }

  const dataBytes = samples.length * 2;
  const buf = new ArrayBuffer(44 + dataBytes);
  const v = new DataView(buf);
  const str = (pos, s) => { for (let i = 0; i < s.length; i++) v.setUint8(pos + i, s.charCodeAt(i)); };
  str(0, 'RIFF');
  v.setUint32(4, 36 + dataBytes, true);
  str(8, 'WAVE');
  str(12, 'fmt ');
  v.setUint32(16, 16, true); // PCM chunk size
  v.setUint16(20, 1, true); // PCM format
  v.setUint16(22, 1, true); // mono
  v.setUint32(24, targetRate, true);
  v.setUint32(28, targetRate * 2, true); // byte rate
  v.setUint16(32, 2, true); // block align
  v.setUint16(34, 16, true); // bits per sample
  str(36, 'data');
  v.setUint32(40, dataBytes, true);

  let pos = 44;
  for (let i = 0; i < samples.length; i++) {
    const s = Math.max(-1, Math.min(1, samples[i]));
    v.setInt16(pos, s < 0 ? s * 0x8000 : s * 0x7fff, true);
    pos += 2;
  }
  return new Uint8Array(buf);
}

/** Format elapsed recording seconds as M:SS. */
function fmtClock(ms) {
  const s = Math.max(0, Math.floor(ms / 1000));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
}

/** Human-friendly message for Web Speech API, getUserMedia, or local fallback error codes. */
export function describeVoiceError(code, detail = '') {
  switch (code) {
    case 'not-allowed':
      return {
        text: 'Microphone access was denied. Allow microphone access in your browser or OS privacy settings, then try again.',
        retry: true,
      };
    case 'service-not-allowed':
      return {
        text: detail || (isDesktop
          ? 'Browser speech service is unavailable in the desktop WebView.'
          : 'Speech recognition service is not allowed in this browser or context.'),
        retry: false,
      };
    case 'audio-capture':
      return {
        text: 'No working microphone was found. Check your input device and try again.',
        retry: true,
      };
    case 'network':
      return {
        text: detail || 'Speech recognition could not reach its network service. Check your connection and try again.',
        retry: true,
      };
    case 'language-not-supported':
      return {
        text: 'The selected speech language is not supported by this browser. Try Browser default.',
        retry: false,
      };
    case 'no-speech':
      return {
        text: 'No speech detected. Speak closer to the mic and try again.',
        retry: true,
        soft: true,
      };
    case 'unsupported':
      return { text: detail || unsupportedReason(), retry: false };
    case 'aborted':
      return null;
    default:
      return { text: detail || `Voice input stopped (${code || 'unknown error'}).`, retry: true };
  }
}

// Track active controller across multiple mounted composers (e.g. project chat + peeked thread)
let activeVoice = null;
const mounted = new Set();
let lastFocused = null;
let androidListener = null;

// Shared live download status from the backend (/api/voice/setup + WS 'voice' events)
let localSetup = null;
const setupSubs = new Set();
let busWired = false;
function watchLocalSetup(fn) {
  if (!busWired) {
    busWired = true;
    listen((ev) => {
      if (ev.kind === 'voice' && ev.data) {
        localSetup = ev.data;
        setupSubs.forEach((f) => f(localSetup));
      }
    });
  }
  setupSubs.add(fn);
  return () => setupSubs.delete(fn);
}

// Native Android SpeechRecognizer bridge callback
if (typeof window !== 'undefined') {
  window.__roamerVoiceEvent = (ev) => {
    try {
      const data = typeof ev === 'string' ? JSON.parse(ev) : ev;
      androidListener?.(data);
    } catch { /* ignore malformed native payload */ }
  };
}

/** Pick the composer that should answer the global voice keyboard shortcut. */
function targetController() {
  if (activeVoice && activeVoice.isConnected()) return activeVoice;
  const ae = document.activeElement;
  for (const c of mounted) if (c.isConnected() && c.contains(ae)) return c;
  if (lastFocused && lastFocused.isConnected()) return lastFocused;
  for (const c of mounted) if (c.isConnected()) return c;
  return null;
}

// Single global keyboard handler for Ctrl+Shift+M (Cmd+Shift+M on macOS) and Escape to cancel
let shortcutWired = false;
let pttKeyDown = false;
function wireGlobalShortcut() {
  if (shortcutWired || typeof document === 'undefined') return;
  shortcutWired = true;
  document.addEventListener('keydown', (e) => {
    if (activeVoice && e.key === 'Escape' && !overlaid()) {
      e.preventDefault();
      e.stopPropagation();
      activeVoice.cancel();
      return;
    }
    if (overlaid()) return;
    if ((e.ctrlKey || e.metaKey) && e.shiftKey && !e.altKey && e.key.toLowerCase() === 'm') {
      const ctrl = targetController();
      if (!ctrl) return;
      e.preventDefault();
      e.stopPropagation();
      if (e.repeat) return;
      if (voiceSettings().mode === 'ptt') {
        pttKeyDown = true;
        ctrl.start();
      } else {
        ctrl.toggle();
      }
    }
  }, true);
  document.addEventListener('keyup', (e) => {
    if (!pttKeyDown) return;
    const k = e.key.toLowerCase();
    if (k === 'm' || k === 'shift' || k === 'control' || k === 'meta') {
      pttKeyDown = false;
      activeVoice?.stop();
    }
  }, true);
  window.addEventListener('blur', () => {
    if (pttKeyDown) {
      pttKeyDown = false;
      activeVoice?.stop();
    }
  });
}

/**
 * Create a voice dictation controller and UI elements for a chat composer.
 */
export function createVoice({ composerEl, ta, onUpdate, onSend }) {
  wireGlobalShortcut();

  const shortcutLabel = plat === 'macos' ? '⌘⇧M' : 'Ctrl+Shift+M';
  const statusEl = h('div', { class: 'cmp-voice', hidden: true });
  const micBtn = h('button', { class: 'ibtn cmp-mic', type: 'button', 'aria-pressed': 'false' });
  const modeBtn = h('button', { class: 'cmp-steps cmp-voice-cfg', type: 'button', title: 'Voice input settings' });

  let state = 'idle'; // 'idle' | 'starting' | 'listening' | 'recording' | 'transcribing'
  let error = null; // { text, retry, soft } | null
  let setupOffer = null; // backend status object when optional offline download is offered or running
  let setupPoll = 0;
  let rec = null;
  let startedAt = 0;
  let timer = 0;
  let snapshot = null; // { before, after, start, end }
  let committed = '';
  let interim = '';
  let resultOffset = 0;
  let lastResultCount = 0;
  let ignoreEnd = false;
  let cancelled = false;
  let suppressAutoSend = false;
  let pointerHolding = false;
  let settingTextarea = false;

  // Local offline recording state (getUserMedia + AudioContext PCM capture)
  let localStream = null;
  let localAudioCtx = null;
  let localProcessor = null;
  let localSource = null;
  let localChunks = [];
  let localSampleRate = 48000;
  let localLevel = 0;
  let localAbort = null;

  const isSupported = () => !!voiceBackend();
  const isActive = () => state !== 'idle';

  function drawMic() {
    const s = voiceSettings();
    const sup = isSupported();
    const active = isActive();
    micBtn.classList.toggle('listening', active);
    micBtn.classList.toggle('starting', state === 'starting' || state === 'transcribing');
    micBtn.classList.toggle('unsupported', !sup);
    micBtn.setAttribute('aria-pressed', String(active));
    micBtn.innerHTML = icon(sup ? 'mic' : 'micOff', 17);
    micBtn.title = !sup
      ? `Voice input unavailable (${unsupportedReason()})`
      : active
        ? `Stop listening (${shortcutLabel}) · Esc to cancel`
        : s.mode === 'ptt'
          ? `Hold to talk (${shortcutLabel}) · Right-click for voice settings`
          : `Voice input (${shortcutLabel}) · Right-click for voice settings`;

    composerEl.classList.toggle('listening', active);

    const modeWord = s.mode === 'ptt' ? 'Hold' : 'Toggle';
    modeBtn.replaceChildren(ic(sup ? 'mic' : 'micOff', 12), h('span', {}, `Voice: ${modeWord}`));
    modeBtn.classList.toggle('own', active || s.auto_send || s.mode === 'ptt');
    modeBtn.title = `Voice input (${modeWord}${s.auto_send ? ', auto-send' : ''}) — ${shortcutLabel}`;
  }

  function meterBars() {
    const lv = Math.min(1, localLevel * 4.5);
    const weights = [0.45, 0.75, 1.0, 0.85, 0.6];
    return h('span', { class: 'cmp-voice-meter', 'aria-hidden': 'true' },
      weights.map((w) => h('i', { style: { height: Math.max(20, Math.round(lv * w * 100)) + '%' } })));
  }

  function drawStatus() {
    if (state === 'transcribing') {
      statusEl.hidden = false;
      statusEl.replaceChildren(
        h('div', { class: 'cmp-voice-bar transcribing', role: 'status', 'aria-live': 'polite' },
          h('span', { class: 'spin', 'aria-hidden': 'true' }),
          h('span', { class: 'cmp-voice-lbl mono' }, 'Transcribing offline…'),
          h('span', { class: 'cmp-voice-live' }, 'Running local whisper.cpp'),
          h('div', { class: 'cmp-voice-acts' },
            h('button', { class: 'cmp-voice-btn', type: 'button', title: 'Cancel transcription (Esc)', onclick: () => cancel() }, 'Cancel'))));
      return;
    }

    if (state !== 'idle') {
      const s = voiceSettings();
      const elapsed = startedAt ? fmtClock(Date.now() - startedAt) : '0:00';
      const isLocalRec = state === 'recording';
      const label = state === 'starting'
        ? 'Starting mic…'
        : s.mode === 'ptt'
          ? `Holding to talk · ${elapsed}`
          : isLocalRec
            ? `Recording (offline) · ${elapsed}`
            : `Listening · ${elapsed}`;
      const preview = isLocalRec
        ? 'Speak now — click Done to transcribe locally'
        : interim.trim() || (committed.trim() ? 'Keep speaking…' : 'Speak now — transcript fills the box');
      statusEl.hidden = false;
      statusEl.replaceChildren(
        h('div', { class: 'cmp-voice-bar', role: 'status', 'aria-live': 'polite' },
          h('i', { class: 'cmp-voice-dot', 'aria-hidden': 'true' }),
          h('span', { class: 'cmp-voice-lbl mono' }, label),
          isLocalRec ? meterBars() : null,
          h('span', { class: 'cmp-voice-live' + (!isLocalRec && interim.trim() ? ' has' : '') }, preview),
          h('div', { class: 'cmp-voice-acts' },
            h('button', { class: 'cmp-voice-btn', type: 'button', title: 'Discard this dictation (Esc)', onclick: () => cancel() }, 'Cancel'),
            s.mode === 'toggle'
              ? h('button', { class: 'cmp-voice-btn done', type: 'button', title: `Finish dictation (${shortcutLabel})`, onclick: () => stop() }, 'Done')
              : null)));
      return;
    }

    if (setupOffer) {
      const busy = setupOffer.state === 'downloading' || setupOffer.state === 'extracting';
      const failed = setupOffer.state === 'error';
      if (busy) {
        const pct = Math.max(0, Math.min(100, Number(setupOffer.pct) || 0));
        statusEl.hidden = false;
        statusEl.replaceChildren(
          h('div', { class: 'cmp-voice-dl', role: 'status', 'aria-live': 'polite' },
            h('div', { class: 'cmp-voice-dl-top' },
              h('span', { class: 'spin', 'aria-hidden': 'true' }),
              h('span', { class: 'cmp-voice-dl-t mono' }, setupOffer.detail || 'Downloading offline voice…'),
              h('span', { class: 'mono faint' }, setupOffer.state === 'extracting' ? 'extracting' : `${pct}%`)),
            h('div', { class: 'cmp-voice-prog' }, h('i', { style: { width: `${pct}%` } }))));
        return;
      }
      if (failed) {
        statusEl.hidden = false;
        statusEl.replaceChildren(
          h('div', { class: 'cmp-voice-err', role: 'alert' },
            ic('warn', 14),
            h('span', {}, setupOffer.error || "Couldn't download offline voice."),
            setupOffer.can_download
              ? h('button', { class: 'cmp-voice-btn', type: 'button', onclick: () => triggerLocalSetup() }, 'Retry')
              : null,
            h('button', { class: 'ibtn xs', type: 'button', title: 'Dismiss', onclick: () => { setupOffer = null; drawStatus(); }, html: icon('close', 12) })));
        return;
      }
      // Optional first-use offer
      const mb = setupOffer.model_mb || 141;
      const missingEngine = !setupOffer.engine_ready;
      const msg = !setupOffer.can_download
        ? (setupOffer.detail || "Offline speech engine (whisper.cpp) isn't installed on this machine.")
        : missingEngine
          ? `Browser speech isn't available here. Download free offline voice (whisper.cpp + ~${mb} MB model, cached once)?`
          : `Download the free offline speech model (~${mb} MB, cached on disk) to transcribe locally?`;
      statusEl.hidden = false;
      statusEl.replaceChildren(
        h('div', { class: 'cmp-voice-err soft', role: 'status' },
          ic('download', 14),
          h('span', {}, msg),
          setupOffer.can_download
            ? h('button', { class: 'cmp-voice-btn done', type: 'button', onclick: () => triggerLocalSetup() }, `Download (~${mb} MB)`)
            : h('button', { class: 'cmp-voice-btn', type: 'button', onclick: () => checkAndStartLocal(voiceSettings()) }, 'Check again'),
          h('button', { class: 'ibtn xs', type: 'button', title: 'Dismiss', onclick: () => { setupOffer = null; drawStatus(); }, html: icon('close', 12) })));
      return;
    }

    if (error) {
      statusEl.hidden = false;
      statusEl.replaceChildren(
        h('div', { class: 'cmp-voice-err' + (error.soft ? ' soft' : ''), role: 'alert' },
          ic(error.soft ? 'info' : 'warn', 14),
          h('span', {}, error.text),
          error.retry ? h('button', { class: 'cmp-voice-btn', type: 'button', onclick: () => { error = null; start(); } }, 'Try again') : null,
          h('button', { class: 'ibtn xs', type: 'button', title: 'Dismiss', onclick: () => { error = null; drawStatus(); }, html: icon('close', 12) })));
      return;
    }

    statusEl.hidden = true;
    statusEl.replaceChildren();
  }

  function applyTranscript() {
    if (!snapshot) return;
    const combined = [committed.trim(), interim.trim()].filter(Boolean).join(' ');
    const { text, caret } = spliceTranscript(snapshot.before, snapshot.after, combined);
    settingTextarea = true;
    ta.value = text;
    try {
      ta.setSelectionRange(caret, caret);
    } catch { /* hidden/unfocused */ }
    onUpdate?.();
    settingTextarea = false;
  }

  function restoreSnapshot() {
    if (!snapshot) return;
    settingTextarea = true;
    ta.value = snapshot.before + snapshot.after;
    try {
      ta.setSelectionRange(snapshot.start, snapshot.end);
    } catch { /* fine */ }
    onUpdate?.();
    settingTextarea = false;
  }

  function releaseLocalAudio() {
    try { localProcessor?.disconnect(); } catch { /* fine */ }
    try { localSource?.disconnect(); } catch { /* fine */ }
    localProcessor = null;
    localSource = null;
    if (localStream) {
      try { localStream.getTracks().forEach((tr) => tr.stop()); } catch { /* fine */ }
      localStream = null;
    }
    if (localAudioCtx) {
      try { localAudioCtx.close(); } catch { /* fine */ }
      localAudioCtx = null;
    }
    localLevel = 0;
  }

  function stopSetupPoll() {
    clearInterval(setupPoll);
    setupPoll = 0;
  }

  function cleanupSession({ keepError = false, keepSetup = false } = {}) {
    clearInterval(timer);
    timer = 0;
    rec = null;
    androidListener = null;
    releaseLocalAudio();
    if (localAbort) {
      try { localAbort.abort(); } catch { /* fine */ }
      localAbort = null;
    }
    if (activeVoice === ctrl) activeVoice = null;
    state = 'idle';
    if (!keepError) error = null;
    if (!keepSetup) setupOffer = null;
    drawMic();
    drawStatus();
  }

  function finishSession() {
    const hadText = Boolean((committed + ' ' + interim).trim());
    const shouldSend = !suppressAutoSend && voiceSettings().auto_send;
    suppressAutoSend = false;
    if (hadText && !cancelled) {
      if (interim.trim()) {
        committed = [committed.trim(), interim.trim()].filter(Boolean).join(' ');
        interim = '';
        applyTranscript();
      }
    }
    snapshot = null;
    committed = '';
    interim = '';
    cleanupSession({ keepError: !!error, keepSetup: !!setupOffer });
    ta.focus();
    if (hadText && !cancelled && shouldSend && !error) {
      onSend?.();
    }
  }

  async function triggerLocalSetup() {
    error = null;
    setupOffer = { ...(setupOffer || {}), state: 'downloading', pct: 0, detail: 'Starting download…', can_download: true };
    drawStatus();
    try {
      const st = await post('voice/setup', {});
      handleSetupUpdate(st);
      if (st.state === 'downloading' || st.state === 'extracting') startSetupPoll();
    } catch (e) {
      setupOffer = { ...(setupOffer || {}), state: 'error', error: e.message || "Couldn't start offline voice download.", can_download: true };
      drawStatus();
    }
  }

  function startSetupPoll() {
    stopSetupPoll();
    setupPoll = setInterval(async () => {
      try {
        const st = await get('voice/status', { quiet: true });
        handleSetupUpdate(st);
      } catch { /* keep waiting for WS or next poll */ }
    }, 1200);
  }

  function handleSetupUpdate(st) {
    if (!st) return;
    if (st.ready && setupOffer) {
      stopSetupPoll();
      setupOffer = null;
      error = null;
      drawMic();
      drawStatus();
      toast('Offline voice is ready — click the mic or press ' + shortcutLabel, { ms: 4200 });
      return;
    }
    if (setupOffer || st.state === 'downloading' || st.state === 'extracting') {
      setupOffer = st;
      if (st.state !== 'downloading' && st.state !== 'extracting') stopSetupPoll();
      drawStatus();
    }
  }

  async function checkAndStartLocal(s) {
    error = null;
    let st;
    try {
      st = await get('voice/status', { quiet: true });
    } catch (e) {
      error = describeVoiceError('unknown', `Couldn't check local voice engine: ${e.message}`);
      cleanupSession({ keepError: true });
      return;
    }
    if (cancelled) return;
    if (st.ready) {
      await startLocalRecording(s);
      return;
    }
    // Not ready yet: present optional download prompt (or ongoing download progress)
    setupOffer = st;
    if (st.state === 'downloading' || st.state === 'extracting') startSetupPoll();
    snapshot = null;
    cleanupSession({ keepSetup: true });
  }

  async function startLocalRecording(s) {
    if (!hasGetUserMedia()) {
      error = describeVoiceError('unsupported');
      cleanupSession({ keepError: true });
      return;
    }
    let stream;
    try {
      stream = await navigator.mediaDevices.getUserMedia({
        audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true },
      });
    } catch (e) {
      const name = e?.name || '';
      const code = (name === 'NotAllowedError' || name === 'PermissionDeniedError' || name === 'SecurityError')
        ? 'not-allowed'
        : (name === 'NotFoundError' || name === 'DevicesNotFoundError' || name === 'NotReadableError')
          ? 'audio-capture'
          : 'unknown';
      error = describeVoiceError(code, e?.message);
      cleanupSession({ keepError: true });
      return;
    }
    if (cancelled) {
      try { stream.getTracks().forEach((tr) => tr.stop()); } catch { /* fine */ }
      return;
    }
    const AC = window.AudioContext || window.webkitAudioContext;
    if (!AC) {
      try { stream.getTracks().forEach((tr) => tr.stop()); } catch { /* fine */ }
      error = describeVoiceError('unsupported', 'Web Audio API is unavailable in this browser.');
      cleanupSession({ keepError: true });
      return;
    }
    try {
      const ctx = new AC();
      const src = ctx.createMediaStreamSource(stream);
      const proc = ctx.createScriptProcessor(4096, 1, 1);
      localStream = stream;
      localAudioCtx = ctx;
      localSource = src;
      localProcessor = proc;
      localSampleRate = ctx.sampleRate || 48000;
      localChunks = [];
      localLevel = 0;

      proc.onaudioprocess = (ev) => {
        if (state !== 'recording') return;
        const ch = ev.inputBuffer.getChannelData(0);
        localChunks.push(new Float32Array(ch));
        let sum = 0;
        for (let i = 0; i < ch.length; i += 4) sum += ch[i] * ch[i];
        localLevel = Math.sqrt(sum / Math.max(1, ch.length / 4));
      };
      src.connect(proc);
      proc.connect(ctx.destination);
      if (ctx.state === 'suspended') ctx.resume?.().catch(() => {});

      state = 'recording';
      startedAt = Date.now();
      clearInterval(timer);
      timer = setInterval(() => state === 'recording' && drawStatus(), 200);
      drawMic();
      drawStatus();
    } catch (e) {
      releaseLocalAudio();
      error = describeVoiceError('audio-capture', e?.message);
      cleanupSession({ keepError: true });
    }
  }

  async function finishLocalRecording({ keepTextNow = false } = {}) {
    const chunks = localChunks;
    const rate = localSampleRate;
    localChunks = [];
    clearInterval(timer);
    timer = 0;
    releaseLocalAudio();

    if (!chunks.length || cancelled) {
      finishSession();
      return;
    }
    if (keepTextNow) {
      // User manually clicked Send before transcription; don't overwrite what they sent
      snapshot = null;
      cleanupSession();
      return;
    }

    state = 'transcribing';
    drawMic();
    drawStatus();

    const wav = encodeWav(chunks, rate, 16000);
    const s = voiceSettings();
    const qs = s.lang ? '?lang=' + encodeURIComponent(s.lang) : '';
    const ctl = new AbortController();
    localAbort = ctl;

    try {
      const res = await api('voice/transcribe' + qs, {
        method: 'POST',
        body: new Blob([wav], { type: 'audio/wav' }),
        raw: true,
        timeout: 90000,
      });
      if (cancelled || localAbort !== ctl) return;
      localAbort = null;
      const text = (res?.text || '').trim();
      if (text) {
        committed = text;
        interim = '';
        applyTranscript();
      } else {
        error = describeVoiceError('no-speech');
      }
      finishSession();
    } catch (e) {
      if (cancelled || localAbort !== ctl) return;
      localAbort = null;
      const msg = e?.message || 'Local transcription failed.';
      // If the engine/model disappeared from disk, surface the setup prompt again
      if (/isn't installed|isn't downloaded/i.test(msg)) {
        snapshot = null;
        cleanupSession();
        checkAndStartLocal(s);
        return;
      }
      error = { text: msg, retry: true };
      finishSession();
    }
  }

  function startWebSpeech(s) {
    const SR = window.SpeechRecognition || window.webkitSpeechRecognition;
    if (!SR) {
      if (hasGetUserMedia()) {
        checkAndStartLocal(s);
        return;
      }
      error = describeVoiceError('unsupported');
      cleanupSession({ keepError: true });
      return;
    }
    const r = new SR();
    rec = r;
    r.continuous = true;
    r.interimResults = true;
    r.maxAlternatives = 1;
    r.lang = s.lang || navigator.language || 'en-US';

    r.onstart = () => {
      if (rec !== r) return;
      state = 'listening';
      startedAt = Date.now();
      clearInterval(timer);
      timer = setInterval(() => state === 'listening' && drawStatus(), 500);
      drawMic();
      drawStatus();
    };

    r.onresult = (ev) => {
      if (rec !== r || cancelled) return;
      lastResultCount = ev.results.length;
      let fin = '';
      let tmp = '';
      for (let i = resultOffset; i < ev.results.length; i++) {
        const res = ev.results[i];
        const piece = res[0]?.transcript || '';
        if (res.isFinal) fin = fin ? `${fin.trimEnd()} ${piece.trim()}` : piece.trim();
        else tmp += piece;
      }
      committed = fin;
      interim = tmp;
      applyTranscript();
      drawStatus();
    };

    r.onerror = (ev) => {
      if (rec !== r) return;
      const code = ev?.error || 'unknown';
      if (cancelled || code === 'aborted') return;
      // Automatic fallback to free local whisper.cpp when Web Speech reports network / service-not-allowed (WebView2)
      if ((code === 'network' || code === 'service-not-allowed') && !committed.trim() && hasGetUserMedia()) {
        webSpeechBlocked = true;
        ignoreEnd = true;
        rec = null;
        try { r.abort(); } catch { /* fine */ }
        clearInterval(timer);
        timer = 0;
        state = 'starting';
        drawMic();
        drawStatus();
        checkAndStartLocal(s);
        return;
      }
      if (code === 'no-speech' && (committed.trim() || interim.trim())) return;
      const desc = describeVoiceError(code, ev?.message);
      if (desc) error = desc;
      drawStatus();
    };

    r.onend = () => {
      if (rec !== r) return;
      if (ignoreEnd) {
        ignoreEnd = false;
        return;
      }
      finishSession();
    };

    try {
      r.start();
    } catch (e) {
      error = describeVoiceError(e?.name === 'NotAllowedError' ? 'not-allowed' : 'unknown', e?.message);
      cleanupSession({ keepError: true });
    }
  }

  function startAndroidSpeech(s) {
    const lang = s.lang || navigator.language || 'en-US';
    androidListener = (ev) => {
      if (!ev || state === 'idle') return;
      if (ev.type === 'ready') {
        state = 'listening';
        startedAt = Date.now();
        clearInterval(timer);
        timer = setInterval(() => state === 'listening' && drawStatus(), 500);
        drawMic();
        drawStatus();
      } else if (ev.type === 'partial' && !cancelled) {
        interim = ev.text || '';
        applyTranscript();
        drawStatus();
      } else if (ev.type === 'final' && !cancelled) {
        const piece = (ev.text || '').trim();
        if (piece) committed = committed ? `${committed.trimEnd()} ${piece}` : piece;
        interim = '';
        applyTranscript();
        finishSession();
      } else if (ev.type === 'error') {
        if (cancelled) return;
        error = describeVoiceError(ev.code || 'unknown', ev.message);
        finishSession();
      } else if (ev.type === 'end') {
        if (!cancelled && state !== 'idle') finishSession();
      }
    };
    try {
      window.RoamerAndroid.startVoice(lang);
    } catch (e) {
      error = describeVoiceError('unknown', e?.message);
      cleanupSession({ keepError: true });
    }
  }

  function start() {
    if (state !== 'idle') return;
    error = null;
    setupOffer = null;
    const backend = voiceBackend();
    if (!backend) {
      error = describeVoiceError('unsupported');
      drawMic();
      drawStatus();
      return;
    }
    if (activeVoice && activeVoice !== ctrl) activeVoice.stop();

    const startPos = typeof ta.selectionStart === 'number' ? ta.selectionStart : ta.value.length;
    const endPos = typeof ta.selectionEnd === 'number' ? ta.selectionEnd : startPos;
    snapshot = {
      before: ta.value.slice(0, startPos),
      after: ta.value.slice(endPos),
      start: startPos,
      end: endPos,
    };
    committed = '';
    interim = '';
    resultOffset = 0;
    lastResultCount = 0;
    cancelled = false;
    suppressAutoSend = false;
    ignoreEnd = false;
    state = 'starting';
    startedAt = Date.now();
    activeVoice = ctrl;
    drawMic();
    drawStatus();

    const s = voiceSettings();
    if (backend === 'android') startAndroidSpeech(s);
    else if (backend === 'local') checkAndStartLocal(s);
    else startWebSpeech(s);
  }

  function stop({ keepTextNow = false } = {}) {
    if (state === 'idle') return;
    cancelled = false;
    if (state === 'recording') {
      finishLocalRecording({ keepTextNow });
      return;
    }
    if (state === 'transcribing') return;
    if (keepTextNow) {
      suppressAutoSend = true;
      if (interim.trim()) {
        committed = [committed.trim(), interim.trim()].filter(Boolean).join(' ');
        interim = '';
        applyTranscript();
      }
      snapshot = null;
      committed = '';
      interim = '';
      if (rec) {
        ignoreEnd = true;
        try { rec.stop(); } catch { /* fine */ }
      } else if (androidListener && window.RoamerAndroid?.stopVoice) {
        try { window.RoamerAndroid.stopVoice(); } catch { /* fine */ }
      }
      cleanupSession();
      return;
    }
    if (rec) {
      try {
        rec.stop();
      } catch {
        finishSession();
      }
    } else if (androidListener && window.RoamerAndroid?.stopVoice) {
      try {
        window.RoamerAndroid.stopVoice();
      } catch {
        finishSession();
      }
    } else {
      finishSession();
    }
  }

  function cancel() {
    if (state === 'idle') return;
    cancelled = true;
    error = null;
    restoreSnapshot();
    snapshot = null;
    committed = '';
    interim = '';
    localChunks = [];
    if (rec) {
      ignoreEnd = true;
      try { rec.abort(); } catch { /* fine */ }
    } else if (androidListener && window.RoamerAndroid?.cancelVoice) {
      try { window.RoamerAndroid.cancelVoice(); } catch { /* fine */ }
    }
    cleanupSession();
    ta.focus();
  }

  function toggle() {
    if (state !== 'idle') stop();
    else start();
  }

  function openMenu(anchor, x, y) {
    const s = voiceSettings();
    const r = anchor.getBoundingClientRect();
    showMenu([
      { header: 'Voice input' },
      { label: 'Click to start / stop (Toggle)', checked: s.mode === 'toggle', kbd: shortcutLabel, run: () => saveVoiceSettings({ mode: 'toggle' }) },
      { label: 'Hold to talk (Push-to-talk)', checked: s.mode === 'ptt', kbd: shortcutLabel, run: () => saveVoiceSettings({ mode: 'ptt' }) },
      { sep: true },
      { label: 'Send automatically when done', checked: s.auto_send, run: () => saveVoiceSettings({ auto_send: !s.auto_send }) },
      {
        label: 'Language',
        icon: 'translate',
        sub: () => LANG_CHOICES.map(([code, label]) => ({
          label,
          checked: s.lang === code,
          run: () => saveVoiceSettings({ lang: code }),
        })),
      },
    ], x ?? r.right, y ?? (r.top - 4), { anchor: x == null ? anchor : undefined });
  }

  // Mic button interactions: toggle on click, or hold in push-to-talk mode; right-click opens settings
  micBtn.addEventListener('click', (e) => {
    if (voiceSettings().mode === 'ptt') {
      if (e.detail === 0) toggle();
      return;
    }
    toggle();
  });
  micBtn.addEventListener('pointerdown', (e) => {
    if (e.button !== 0 || voiceSettings().mode !== 'ptt') return;
    e.preventDefault();
    pointerHolding = true;
    try { micBtn.setPointerCapture(e.pointerId); } catch { /* fine */ }
    start();
  });
  const endPointerHold = (e) => {
    if (!pointerHolding) return;
    pointerHolding = false;
    try {
      if (micBtn.hasPointerCapture?.(e.pointerId)) micBtn.releasePointerCapture(e.pointerId);
    } catch { /* fine */ }
    stop();
  };
  micBtn.addEventListener('pointerup', endPointerHold);
  micBtn.addEventListener('pointercancel', endPointerHold);
  micBtn.addEventListener('contextmenu', (e) => {
    e.preventDefault();
    openMenu(micBtn, e.clientX, e.clientY);
  });
  modeBtn.addEventListener('click', () => openMenu(modeBtn));

  // If the person types manually while listening, rebase so we never overwrite their edits
  const onManualInput = () => {
    if (settingTextarea || state === 'idle') return;
    const pos = typeof ta.selectionStart === 'number' ? ta.selectionStart : ta.value.length;
    snapshot = { before: ta.value.slice(0, pos), after: ta.value.slice(pos), start: pos, end: pos };
    committed = '';
    interim = '';
    resultOffset = lastResultCount;
  };
  const onFocus = () => { lastFocused = ctrl; };
  ta.addEventListener('input', onManualInput);
  ta.addEventListener('focus', onFocus);

  const unwatch = watch((w) => {
    if (w === 'settings' || w === 'all') {
      drawMic();
      if (state !== 'idle') drawStatus();
    }
  });
  const unwatchSetup = watchLocalSetup(handleSetupUpdate);

  const ctrl = {
    micBtn,
    modeBtn,
    statusEl,
    start,
    stop,
    cancel,
    toggle,
    isListening: () => state !== 'idle',
    isConnected: () => composerEl.isConnected,
    contains: (node) => !!node && composerEl.contains(node),
    destroy: () => {
      if (state !== 'idle') cancel();
      stopSetupPoll();
      unwatch();
      unwatchSetup();
      mounted.delete(ctrl);
      if (lastFocused === ctrl) lastFocused = null;
      ta.removeEventListener('input', onManualInput);
      ta.removeEventListener('focus', onFocus);
    },
  };

  mounted.add(ctrl);
  drawMic();
  drawStatus();
  return ctrl;
}

const attached = new WeakMap();

/** Attach voice controls to a rendered `.composer` element (idempotent). */
export function attachComposerVoice(composerEl, { onSend } = {}) {
  if (!composerEl || attached.has(composerEl)) return attached.get(composerEl) || null;
  const row = composerEl.querySelector('.cmp-row');
  const ta = composerEl.querySelector('.cmp-in');
  const sendBtn = composerEl.querySelector('.cmp-send');
  const hint = composerEl.querySelector('.cmp-hint');
  if (!row || !ta || !sendBtn) return null;

  const ctrl = createVoice({
    composerEl,
    ta,
    onUpdate: () => ta.dispatchEvent(new Event('input', { bubbles: true })),
    onSend: onSend || (() => sendBtn.click()),
  });

  // Insert live listening / error banner above the input row, mic button beside Send, and mode button in hint bar
  composerEl.insertBefore(ctrl.statusEl, row);
  row.insertBefore(ctrl.micBtn, sendBtn);
  if (hint) {
    const stepsBtn = hint.querySelector('.cmp-steps');
    if (stepsBtn) hint.insertBefore(ctrl.modeBtn, stepsBtn);
    else hint.append(ctrl.modeBtn);
  }

  // Stop listening cleanly (keeping the dictated text) if the person manually presses Enter or clicks Send
  const stopOnSendClick = () => ctrl.isListening() && ctrl.stop({ keepTextNow: true });
  const stopOnEnter = (e) => {
    if (e.key === 'Enter' && !e.shiftKey && !e.isComposing && ctrl.isListening()) {
      ctrl.stop({ keepTextNow: true });
    }
  };
  sendBtn.addEventListener('click', stopOnSendClick, true);
  ta.addEventListener('keydown', stopOnEnter, true);

  const origDestroy = ctrl.destroy;
  ctrl.destroy = () => {
    sendBtn.removeEventListener('click', stopOnSendClick, true);
    ta.removeEventListener('keydown', stopOnEnter, true);
    ctrl.statusEl.remove();
    ctrl.micBtn.remove();
    ctrl.modeBtn.remove();
    origDestroy();
    attached.delete(composerEl);
  };

  attached.set(composerEl, ctrl);
  return ctrl;
}
