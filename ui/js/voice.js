// Voice input for chat composers: live speech-to-text into the textarea via the browser Web Speech API
// (or the Android WebView native SpeechRecognizer bridge when running in Model Roamer for Android).
// Supports toggle and push-to-talk modes, live transcript preview, cancel (restoring prior text),
// keyboard shortcut (Ctrl+Shift+M), clear permission/unsupported errors, and persisted settings.

import { h } from './util.js';
import { icon } from './icons.js';
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

/** Available speech recognition backend: 'android' | 'webspeech' | null. */
export function voiceBackend() {
  if (typeof window === 'undefined') return null;
  if (window.RoamerAndroid && typeof window.RoamerAndroid.startVoice === 'function') {
    try {
      if (!window.RoamerAndroid.hasVoice || window.RoamerAndroid.hasVoice()) return 'android';
    } catch { /* fall through */ }
  }
  if (window.SpeechRecognition || window.webkitSpeechRecognition) return 'webspeech';
  return null;
}

export function unsupportedReason() {
  if (typeof window !== 'undefined' && window.isSecureContext === false) {
    return 'Voice input needs HTTPS or localhost. Open Model Roamer on this computer or over HTTPS (such as Tailscale HTTPS).';
  }
  return 'This browser does not support Web Speech recognition. Try Chrome, Edge, or Safari.';
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

/** Format elapsed recording seconds as M:SS. */
function fmtClock(ms) {
  const s = Math.max(0, Math.floor(ms / 1000));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
}

/** Human-friendly message for Web Speech API or native bridge error codes. */
export function describeVoiceError(code, detail = '') {
  switch (code) {
    case 'not-allowed':
      return {
        text: 'Microphone access was denied. Allow microphone access in your browser or OS privacy settings, then try again.',
        retry: true,
      };
    case 'service-not-allowed':
      return {
        text: isDesktop
          ? 'Speech recognition service is blocked in the desktop WebView. Use Win+H for Windows voice typing, or open Model Roamer in Chrome or Edge.'
          : 'Speech recognition service is not allowed in this browser or context.',
        retry: false,
      };
    case 'audio-capture':
      return {
        text: 'No working microphone was found. Check your input device and try again.',
        retry: true,
      };
    case 'network':
      return {
        text: isDesktop
          ? 'Speech service could not be reached from the desktop WebView. Use Win+H for built-in Windows voice typing, or open Model Roamer in Chrome or Edge.'
          : 'Speech recognition could not reach its network service. Check your connection and try again.',
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
 *
 * Returns:
 * - `micBtn`: mic icon button for `.cmp-row`
 * - `modeBtn`: compact mode/settings button for `.cmp-hint`
 * - `statusEl`: live listening strip + inline error banner for `.composer`
 * - `stop()`: stop listening (keeping transcript) — call before sending
 * - `cancel()`: abort listening and restore the pre-dictation textarea text
 * - `destroy()`: clean up listeners & any active mic session on unmount
 */
export function createVoice({ composerEl, ta, onUpdate, onSend }) {
  wireGlobalShortcut();

  const shortcutLabel = plat === 'macos' ? '⌘⇧M' : 'Ctrl+Shift+M';
  const statusEl = h('div', { class: 'cmp-voice', hidden: true });
  const micBtn = h('button', { class: 'ibtn cmp-mic', type: 'button', 'aria-pressed': 'false' });
  const modeBtn = h('button', { class: 'cmp-steps cmp-voice-cfg', type: 'button', title: 'Voice input settings' });

  let state = 'idle'; // 'idle' | 'starting' | 'listening'
  let error = null; // { text, retry, soft } | null
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

  const isSupported = () => !!voiceBackend();

  function drawMic() {
    const s = voiceSettings();
    const sup = isSupported();
    const active = state !== 'idle';
    micBtn.classList.toggle('listening', active);
    micBtn.classList.toggle('starting', state === 'starting');
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

  function drawStatus() {
    if (state !== 'idle') {
      const s = voiceSettings();
      const elapsed = startedAt ? fmtClock(Date.now() - startedAt) : '0:00';
      const label = state === 'starting'
        ? 'Starting mic…'
        : s.mode === 'ptt'
          ? `Holding to talk · ${elapsed}`
          : `Listening · ${elapsed}`;
      const preview = interim.trim() || (committed.trim() ? 'Keep speaking…' : 'Speak now — transcript fills the box');
      statusEl.hidden = false;
      statusEl.replaceChildren(
        h('div', { class: 'cmp-voice-bar', role: 'status', 'aria-live': 'polite' },
          h('i', { class: 'cmp-voice-dot', 'aria-hidden': 'true' }),
          h('span', { class: 'cmp-voice-lbl mono' }, label),
          h('span', { class: 'cmp-voice-live' + (interim.trim() ? ' has' : '') }, preview),
          h('div', { class: 'cmp-voice-acts' },
            h('button', { class: 'cmp-voice-btn', type: 'button', title: 'Discard this dictation (Esc)', onclick: () => cancel() }, 'Cancel'),
            s.mode === 'toggle'
              ? h('button', { class: 'cmp-voice-btn done', type: 'button', title: `Finish dictation (${shortcutLabel})`, onclick: () => stop() }, 'Done')
              : null)));
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

  function cleanupSession({ keepError = false } = {}) {
    clearInterval(timer);
    timer = 0;
    rec = null;
    androidListener = null;
    if (activeVoice === ctrl) activeVoice = null;
    state = 'idle';
    if (!keepError) error = null;
    drawMic();
    drawStatus();
  }

  function finishSession() {
    const hadText = Boolean((committed + ' ' + interim).trim());
    const shouldSend = !suppressAutoSend && voiceSettings().auto_send;
    suppressAutoSend = false;
    if (hadText && !cancelled) {
      // Promote any lingering interim words on clean stop
      if (interim.trim()) {
        committed = [committed.trim(), interim.trim()].filter(Boolean).join(' ');
        interim = '';
        applyTranscript();
      }
    }
    snapshot = null;
    committed = '';
    interim = '';
    cleanupSession({ keepError: !!error });
    ta.focus();
    if (hadText && !cancelled && shouldSend && !error) {
      onSend?.();
    }
  }

  function startWebSpeech(s) {
    const SR = window.SpeechRecognition || window.webkitSpeechRecognition;
    if (!SR) {
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
      // Ignore benign no-speech if we already captured text
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
    else startWebSpeech(s);
  }

  function stop({ keepTextNow = false } = {}) {
    if (state === 'idle') return;
    cancelled = false;
    if (keepTextNow) {
      suppressAutoSend = true;
      // Commit text immediately before a manual Send/Enter clears the textarea
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
        try {
          rec.stop();
        } catch { /* fine */ }
      } else if (androidListener && window.RoamerAndroid?.stopVoice) {
        try {
          window.RoamerAndroid.stopVoice();
        } catch { /* fine */ }
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
    if (rec) {
      ignoreEnd = true;
      try {
        rec.abort();
      } catch { /* fine */ }
    } else if (androidListener && window.RoamerAndroid?.cancelVoice) {
      try {
        window.RoamerAndroid.cancelVoice();
      } catch { /* fine */ }
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
      // Keyboard activation (Enter/Space on focused button) still works as toggle fallback
      if (e.detail === 0) toggle();
      return;
    }
    toggle();
  });
  micBtn.addEventListener('pointerdown', (e) => {
    if (e.button !== 0 || voiceSettings().mode !== 'ptt') return;
    e.preventDefault();
    pointerHolding = true;
    try {
      micBtn.setPointerCapture(e.pointerId);
    } catch { /* fine */ }
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
      unwatch();
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

