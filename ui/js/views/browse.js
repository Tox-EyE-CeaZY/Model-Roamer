// The app's own browser: pages that live on the machine Model Roamer runs on (a dev server or preview an agent
// started). On that machine they open directly; from another device the server fetches them and routes them here.

import { h } from '../util.js';
import { icon } from '../icons.js';
import { post } from '../api.js';
import { S } from '../store.js';
import { back } from '../app.js';
import { isPop, isLocalUrl, openOutside, setTitle } from '../native.js';
import { toast } from '../ui.js';
import { ic, errorBox } from '../common.js';

export default async function browse(el, r) {
  let url = r.q.u || '';
  const addr = h('input', { class: 'br-addr', spellcheck: 'false', placeholder: 'http://localhost:3000', value: url });
  const frame = h('iframe', { class: 'br-frame', referrerpolicy: 'no-referrer', allow: 'clipboard-write; fullscreen' });
  const note = h('div', { class: 'br-note faint small', hidden: true });
  let shown = '', outside = '';

  /** Where the frame loads this address from: straight, or through the server when it isn't on this machine. */
  async function load(u, bust) {
    u = (u || '').trim();
    if (u && !/^https?:\/\//i.test(u)) u = 'http://' + u;
    if (!u) return;
    if (!isLocalUrl(u)) {
      addr.value = u;
      note.hidden = false;
      note.textContent = 'This browser is for pages on this machine. Opening it in your own browser.';
      return openOutside(u);
    }
    note.hidden = true;
    url = u;
    addr.value = u;
    try {
      history.replaceState(history.state, '', '#/browse?u=' + encodeURIComponent(u));
    } catch { /* fine */ }
    setTitle('Browser · ' + u.replace(/^https?:\/\//, ''));
    try {
      if (S.me?.local) {
        shown = u;
        outside = u;
      } else {
        const f = await post('forward', { url: u });
        shown = f.path;
        outside = location.origin + f.path;
      }
    } catch (e) {
      return toast(e.message, { err: true });
    }
    frame.src = bust ? shown + (shown.includes('?') ? '&' : '?') + '_r=' + Date.now() : shown;
  }

  addr.onkeydown = (e) => e.key === 'Enter' && load(addr.value);
  el.classList.add('browse');
  el.append(
    h('div', { class: 'br-bar' },
      isPop ? null : h('button', { class: 'ibtn', title: 'Back (Alt+Left)', onclick: () => back('#/'), html: icon('back', 16) }),
      h('button', { class: 'ibtn', title: 'Reload', onclick: () => load(url, true), html: icon('reload', 16) }),
      addr,
      h('button', { class: 'ibtn', title: S.me?.local ? 'Open in your browser' : 'Open in your browser (a link anyone who has it can use)', onclick: () => outside && openOutside(outside), html: icon('external', 16) })),
    note,
    frame);
  if (!url) return el.append(errorBox({ message: 'Type an address above, like http://localhost:3000.' }));
  await load(url);
}
