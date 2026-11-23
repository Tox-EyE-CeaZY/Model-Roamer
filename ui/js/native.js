// The desktop shell (src/desktop.rs) puts window.__roamerDesktop in every window it opens and listens on window.ipc.
// In a plain browser (a paired phone or laptop) none of that exists and the app runs as a web page.

export const desk = window.__roamerDesktop || null;
export const isDesktop = !!desk && typeof window.ipc?.postMessage === 'function';
export const plat = desk?.platform || (navigator.userAgent.includes('Windows') ? 'windows' : navigator.userAgent.includes('Mac') ? 'macos' : 'linux');
/** This window shows one thread or artifact on its own (a pop-out). */
export const isPop = new URLSearchParams(location.search).has('pop');

const handlers = {};
export const onNative = (t, fn) => (handlers[t] ||= []).push(fn);
window.__native = (m) => (handlers[m?.t] || []).forEach((fn) => fn(m));

export function native(cmd, data = {}) {
  if (isDesktop) window.ipc.postMessage(JSON.stringify({ cmd, ...data }));
}

export const win = (op, extra) => native('win', { op, ...extra });

const LOCAL_URL = /^https?:\/\/(localhost|127\.0\.0\.1|\[::1\]|0\.0\.0\.0)(:\d+)?(\/|\?|#|$)/i;

/** A page served on the machine Model Roamer runs on (a dev server or preview an agent started). */
export const isLocalUrl = (url) => LOCAL_URL.test(String(url || ''));

/** Open a link outside the app: the person's own browser (or a new tab when we are the browser). */
export function openOutside(url) {
  if (window.RoamerAndroid) window.RoamerAndroid.openExternal(url);
  else if (isDesktop) native('external', { url });
  else window.open(url, '_blank', 'noopener');
}

/**
 * Open a link. Pages on the server's own localhost can't be reached from another device as "localhost", so they open
 * in the app's browser, which has the server route them; everything else opens in the person's own browser.
 */
export function openExternal(url) {
  if (isLocalUrl(url)) return openBrowser(url);
  openOutside(url);
}

/** The app's own browser, in a window of its own when this one is a pop-out. */
export function openBrowser(url) {
  const hash = '#/browse?u=' + encodeURIComponent(url);
  if (isPop) popOut(hash, { key: 'browse', w: 1100, h: 800 });
  else location.hash = hash;
}

/**
 * Open part of the app in a window of its own (a thread, an artifact). Same key, same window: asking again focuses it.
 * Without the desktop shell it opens a browser window instead.
 */
export function popOut(hash, { key, w = 980, h = 760 } = {}) {
  const path = '/?pop=1' + hash;
  if (isDesktop) native('open', { path, key: key || hash, w, h });
  else window.open(path, key || hash, `popup,width=${w},height=${h}`);
}

export function setTitle(text) {
  document.title = text;
  native('title', { text });
}
