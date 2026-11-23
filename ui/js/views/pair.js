// Pairing a browser (a phone, a laptop) with this Model Roamer: a one-time code from the server's screen turns into
// a sign-in cookie for this browser. Desktop apps pair under Settings › Remote access › Connect to a server.

import { h } from '../util.js';
import { get, post } from '../api.js';
import { isDesktop } from '../native.js';
import { setTitle } from '../native.js';
import { ic, field, input } from '../common.js';
import { MARK } from '../icons.js';

export default async function pair(el, r) {
  setTitle('Pair · Model Roamer');
  document.documentElement.classList.add('pairing');
  let hello = {};
  try {
    hello = await get('hello', { quiet: true });
  } catch { /* shown below */ }
  const code = input(r.q.code || '', { placeholder: 'ABCD-EFGH', class: 'input code-in', autocomplete: 'one-time-code', autocapitalize: 'characters', spellcheck: 'false' });
  const ua = navigator.userAgent;
  const guess = /iPhone/.test(ua) ? 'iPhone' : /iPad/.test(ua) ? 'iPad' : /Android/.test(ua) ? 'Android phone' : /Mac/.test(ua) ? 'Mac' : /Windows/.test(ua) ? 'Windows PC' : 'Browser';
  const name = input(guess, { placeholder: 'What is this device?' });
  const err = h('div', { class: 'dlg-err', hidden: !r.q.expired }, r.q.expired ? 'That sign-in link has expired. Pair again with a fresh code.' : '');
  const btn = h('button', { class: 'btn primary big', type: 'submit' }, 'Pair this device');
  const form = h('form', { class: 'pair-form', onsubmit: async (e) => {
    e.preventDefault();
    err.hidden = true;
    btn.disabled = true;
    try {
      const res = await post('pair', { code: code.value, name: name.value.trim() }, { quiet: true });
      location.href = '/auth?token=' + encodeURIComponent(res.token) + '&next=' + encodeURIComponent('/#/');
    } catch (x) {
      err.textContent = x.message;
      err.hidden = false;
      btn.disabled = false;
    }
  } }, field('Pairing code', code), field('This device', name), err, btn);
  el.append(h('div', { class: 'pair-page' },
    h('div', { class: 'pair-card cut' },
      h('div', { class: 'about-mark', html: `<svg viewBox="0 0 40 40" width="52" height="52">${MARK}</svg>` }),
      h('h1', {}, hello.name ? `Pair with ${hello.name}` : 'Pair with Model Roamer'),
      isDesktop
        ? h('p', { class: 'lede small' }, 'This window lost its sign-in to the local Model Roamer. Quit the app from the tray and start it again; that signs every window back in.')
        : h('p', { class: 'lede small' }, 'On the machine running Model Roamer, open Settings › Remote access and make a pairing code (or run “model-roamer pair” in its terminal). Codes work once and expire after 10 minutes.'),
      form,
      h('p', { class: 'faint small' }, ic('lock', 12), ' Your device gets its own key, which you can remove from the server at any time.'))));
  setTimeout(() => (code.value ? name : code).focus(), 50);
  return () => document.documentElement.classList.remove('pairing');
}
