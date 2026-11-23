// Settings: look, windows, GitHub, remote access (sharing this machine, pairing devices, connecting to a server),
// and about (updates, logs, the rest of the suite). Everything saves as soon as it changes.

import { h, ago, debounce } from '../util.js';
import { icon } from '../icons.js';
import { get, post, del, listen } from '../api.js';
import { S, watch, setSetting } from '../store.js';
import { go, switchTo } from '../app.js';
import { isDesktop, native, openExternal } from '../native.js';
import { dialog, toast } from '../ui.js';
import { runStep } from '../term.js';
import { ic, field, input, select, seg, toggle, loading, errorBox, empty, kicker, copyable, timeAgo, pickFolder } from '../common.js';

const SECTIONS = [
  ['look', 'Appearance', 'brush'],
  ['windows', 'Windows', 'window'],
  ['github', 'Git and GitHub', 'branch'],
  ['remote', 'Remote access', 'wifi'],
  ['about', 'About and updates', 'info'],
];

export default async function settings(el, r) {
  const sec = SECTIONS.some(([k]) => k === r.section) ? r.section : 'look';
  const body = h('div', { class: 'set-body' });
  el.append(h('div', { class: 'page-h' }, h('div', {}, h('h1', {}, 'Settings'), h('p', { class: 'lede' }, 'Saved as you change them.'))),
    h('div', { class: 'set' }, h('nav', { class: 'set-nav' }, SECTIONS.map(([k, label, ico]) => h('a', { class: 'set-i' + (k === sec ? ' on' : ''), href: '#/settings/' + k }, ic(ico, 15), label))), body));
  return ({ look, windows, github, remote, about })[sec](body);
}

const save = (p) => setSetting(p).catch((e) => toast(e.message, { err: true }));

// ------------------------------------------------------------ appearance
function look(el) {
  const s = S.settings;
  const hues = [128, 95, 60, 38, 0, 330, 290, 262, 205, 175];
  const hueRow = h('div', { class: 'swatches' }, hues.map((hh) => h('button', { class: 'sw' + (hh === s.hue ? ' on' : ''), style: { '--gh': hh }, title: 'Hue ' + hh, onclick: (e) => {
    hueRow.querySelectorAll('.sw').forEach((x) => x.classList.remove('on'));
    e.currentTarget.classList.add('on');
    slider.value = hh;
    save({ hue: hh });
  } })));
  const slider = h('input', { type: 'range', min: 0, max: 359, value: s.hue ?? 128 });
  const later = debounce((v) => save({ hue: v }), 250);
  slider.oninput = () => {
    document.documentElement.style.setProperty('--hue', slider.value);
    later(+slider.value);
  };
  el.replaceChildren(
    h('section', { class: 'panel cut' }, kicker('Theme'),
      field('Theme', seg([['night', 'Night'], ['paper', 'Paper'], ['violet', 'Violet']], s.theme || 'night', (v) => save({ theme: v }))),
      field('Accent', h('div', {}, hueRow, slider)),
      field('Density', seg([['cozy', 'Cozy'], ['compact', 'Compact']], s.density || 'cozy', (v) => save({ density: v })))),
    h('section', { class: 'panel cut' }, kicker('Atmosphere'),
      toggle(s.topo !== false, (v) => save({ topo: v }), { label: 'Contour map behind everything' }),
      toggle(s.grain !== false, (v) => save({ grain: v }), { label: 'Paper grain' }),
      toggle(s.motion !== false, (v) => save({ motion: v }), { label: 'Motion (off also follows your system’s reduced-motion setting)' })));
}

// ------------------------------------------------------------ windows
function windows(el) {
  const s = S.settings;
  el.replaceChildren(
    h('section', { class: 'panel cut' }, kicker('Threads'),
      toggle(!!s.popout_threads, (v) => save({ popout_threads: v }), { label: 'Open threads in their own windows (like popping out an email)' }),
      h('p', { class: 'faint small' }, 'Either way: Ctrl+Shift+O, the pop-out button, or a middle click on a thread card opens it in a window of its own. Each window remembers where it was.')),
    isDesktop ? h('section', { class: 'panel cut' }, kicker('This app'),
      toggle(s.restore_windows !== false, (v) => save({ restore_windows: v }), { label: 'Reopen the windows I had open last time' }),
      toggle(s.close_to_tray !== false, (v) => save({ close_to_tray: v }), { label: 'Keep running in the tray when I close the window while agents are working or sharing is on' })) : null);
}

// ------------------------------------------------------------ GitHub
async function github(el) {
  const s = S.settings;
  const who = h('div', { class: 'gh-who' }, loading('Checking GitHub'));
  const tok = input(s.github_token || '', { type: 'password', placeholder: 'ghp_… or github_pat_…', autocomplete: 'off' });
  const name = input(s.git_name || '', { placeholder: 'Your Name (empty: git’s own setting)' });
  const email = input(s.git_email || '', { placeholder: 'you@example.com' });
  name.onchange = () => save({ git_name: name.value.trim() });
  email.onchange = () => save({ git_email: email.value.trim() });
  async function check() {
    try {
      const w = await get('github');
      who.replaceChildren(w.connected
        ? h('div', { class: 'gh-ok' }, w.avatar ? h('img', { src: w.avatar, alt: '', width: 36, height: 36, referrerpolicy: 'no-referrer' }) : ic('check', 20), h('div', {}, h('b', {}, w.login), h('small', { class: 'faint' }, w.name || 'Signed in')))
        : h('div', { class: 'gh-no' }, ic('warn', 18), h('div', {}, h('b', {}, 'Not signed in'), h('small', { class: 'faint' }, 'Pull requests and private clones need GitHub. Sign in with the GitHub CLI, or paste a token.'))));
    } catch (e) {
      who.replaceChildren(errorBox(e));
    }
  }
  el.replaceChildren(
    h('section', { class: 'panel cut' }, kicker('GitHub'),
      who,
      h('div', { class: 'row' },
        h('button', { class: 'btn primary', onclick: async () => {
          const st = await get('setup').catch(() => null);
          const gh = st?.tools.find((t) => t.id === 'gh');
          if (!gh?.installed) {
            if (await dialog({ title: 'Install the GitHub CLI first', text: 'Signing in through the browser uses GitHub’s own CLI. Install it now?', ok: 'Install' })) await runStep({ what: 'tool', agent: 'gh' });
            else return;
          }
          await runStep({ what: 'tool-login', agent: 'gh', title: 'Sign in to GitHub' });
          check();
        } }, ic('login', 14), 'Sign in with the browser')),
      field('Or a personal access token', h('div', { class: 'row' }, tok, h('button', { class: 'btn', onclick: async () => {
        try {
          await post('github', { token: tok.value.trim() });
          toast(tok.value.trim() ? 'Token saved' : 'Token removed');
          check();
        } catch (e) {
          toast(e.message, { err: true });
        }
      } }, 'Save')), 'Needs repo access (classic: “repo”; fine-grained: Contents and Pull requests, read and write). It is kept on this machine and never shown again.')),
    h('section', { class: 'panel cut' }, kicker('Git'),
      h('div', { class: 'row2' }, field('Commit name', name), field('Commit email', email)),
      toggle(s.worktrees !== false, (v) => save({ worktrees: v }), { label: 'Give each worker its own branch and folder (git worktree), so agents never edit the same files at once' }),
      toggle(!!s.auto_push, (v) => save({ auto_push: v }), { label: 'Push workers’ branches to GitHub as they commit' })));
  check();
}

// ------------------------------------------------------------ remote access
async function remote(el) {
  const shareBox = h('section', { class: 'panel cut' });
  const pairBox = h('section', { class: 'panel cut' });
  const devBox = h('section', { class: 'panel cut' });
  const tsBox = h('section', { class: 'panel cut' });
  const connBox = h('section', { class: 'panel cut' });
  const guide = h('section', { class: 'panel cut guide' });
  el.replaceChildren(shareBox, pairBox, devBox, tsBox, S.me.local ? connBox : null, guide);

  let net = null;
  async function loadNet() {
    try {
      net = await get('network');
    } catch (e) {
      shareBox.replaceChildren(errorBox(e, loadNet));
      return;
    }
    drawShare();
    drawTs();
  }

  function drawShare() {
    const s = S.settings.server || {};
    const port = input(s.port ?? 4630, { type: 'number', class: 'input num', min: 1, max: 65535 });
    const name = input(s.name || '', { placeholder: net?.hostname || 'This machine' });
    const pub = input(s.public_url || '', { placeholder: 'https://roamer.example.com' });
    const bind = input(s.bind || '0.0.0.0');
    const cert = input(s.tls_cert || '', { placeholder: 'C:\\certs\\fullchain.pem' });
    const key = input(s.tls_key || '', { placeholder: 'C:\\certs\\privkey.pem' });
    const apply = async (p) => {
      try {
        await setSetting({ server: p });
        await loadNet();
      } catch (e) {
        toast(e.message, { err: true, ms: 8000 });
        loadNet();
      }
    };
    port.onchange = () => apply({ port: +port.value || 4630 });
    name.onchange = () => apply({ name: name.value.trim() });
    pub.onchange = () => apply({ public_url: pub.value.trim().replace(/\/$/, '') });
    bind.onchange = () => apply({ bind: bind.value.trim() || '0.0.0.0' });
    cert.onchange = () => apply({ tls_cert: cert.value.trim() });
    key.onchange = () => apply({ tls_key: key.value.trim() });
    const headless = S.server.headless;
    const urls = net?.urls || [];
    shareBox.replaceChildren(
      kicker(headless ? 'This server' : 'Let other devices in', net?.listening ? h('span', { class: 'status live' }, h('i', { class: 'pulse' }), 'Listening ' + (net.listening_at || '')) : h('span', { class: 'status faint' }, 'Off')),
      headless ? h('p', { class: 'faint small' }, 'Running as a server: phones, laptops and the desktop app connect here after pairing.') : toggle(!!s.share, (v) => apply({ share: v }), { label: 'Share this computer: run work here for my other devices' }),
      h('p', { class: 'faint small' }, 'Everything stays on your machines: devices talk to this one directly (over your network, Tailscale, WireGuard or your own domain). Each device pairs once with a one-time code.'),
      net?.listening ? h('div', { class: 'addr-list' }, kicker('Addresses to use'), urls.map((u) => h('div', { class: 'addr' }, h('span', { class: 'tag' }, u.via), h('small', { class: 'faint' }, u.label), copyable(u.url)))) : null,
      h('details', { class: 'adv' }, h('summary', {}, 'Address, name and HTTPS'),
        h('div', { class: 'row2' }, field('Name others see', name), field('Port', port)),
        field('Listen on', bind, '0.0.0.0 is every network. A single address (your Tailscale or WireGuard IP) keeps it off the others.'),
        field('Your domain', pub, 'If a reverse proxy (Caddy, nginx, Cloudflare Tunnel) serves this machine at your domain, put the address here; it is shown first and used in pairing links.'),
        h('div', { class: 'row2' }, field('TLS certificate (PEM)', cert), field('TLS key (PEM)', key)),
        h('small', { class: 'faint' }, 'With a certificate and key, this machine serves HTTPS itself. Leave them empty behind a proxy or with Tailscale HTTPS.')));
  }

  let pairTimer = 0;
  function drawPair(p) {
    clearInterval(pairTimer);
    if (!p) {
      pairBox.replaceChildren(kicker('Pair a device'),
        h('p', { class: 'faint small' }, 'Make a code, then on the other device open one of the addresses above (or scan the code) and type it in. In another Model Roamer: Remote access › Connect to a server.'),
        h('button', { class: 'btn primary', onclick: async () => {
          if (!net?.listening && !S.server.headless) toast('Turn on sharing first so other devices can reach this one.', { err: true });
          try {
            drawPair(await post('pairing'));
          } catch (e) {
            toast(e.message, { err: true });
          }
        } }, ic('qr', 14), 'Make a pairing code'));
      return;
    }
    const until = Date.now() + p.expires_in * 1000;
    const left = h('span', { class: 'mono faint' });
    const tick = () => {
      const s = Math.max(0, Math.round((until - Date.now()) / 1000));
      left.textContent = s ? `expires in ${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}` : 'expired';
      if (!s) drawPair(null);
    };
    pairBox.replaceChildren(kicker('Pair a device', left),
      h('div', { class: 'pair' },
        h('div', { class: 'pair-qr', html: p.qr || '' }),
        h('div', {}, h('div', { class: 'pair-code' }, p.code), h('p', { class: 'faint small' }, 'Single use. Scan the square with a phone camera, or open the address on the device and type the code.'), copyable(p.link), h('button', { class: 'btn sm', onclick: () => drawPair(null) }, 'Done'))));
    tick();
    pairTimer = setInterval(tick, 1000);
  }

  async function drawDevices() {
    let r;
    try {
      r = await get('devices');
    } catch (e) {
      return devBox.replaceChildren(errorBox(e, drawDevices));
    }
    devBox.replaceChildren(kicker('Paired devices', h('b', { class: 'count' }, r.devices.length)),
      r.devices.length ? h('div', { class: 'dev-list' }, r.devices.map((d) => h('div', { class: 'dev' }, ic('desktop', 15), h('div', {}, h('b', {}, d.name, d.id === r.me ? h('span', { class: 'tag' }, 'this one') : null), h('small', { class: 'faint' }, `paired ${ago(d.created_at)} · last seen ${d.last_seen ? ago(d.last_seen) : 'never'}${d.last_ip ? ' from ' + d.last_ip : ''}`)),
        h('button', { class: 'btn xs danger', onclick: async () => {
          if (!(await dialog({ title: `Remove ${d.name}?`, text: 'It is signed out at once and needs a new code to come back.', ok: 'Remove', danger: true }))) return;
          await del('devices/' + d.id);
          drawDevices();
        } }, 'Remove'))))
        : h('p', { class: 'faint small' }, 'No devices yet.'));
  }

  function drawTs() {
    const ts = net?.tailscale || {};
    tsBox.replaceChildren(kicker('Tailscale', ts.running ? h('span', { class: 'status acc' }, 'Connected') : ts.installed ? h('span', { class: 'status dim' }, 'Not connected') : h('span', { class: 'status faint' }, 'Not installed')),
      h('p', { class: 'faint small' }, 'The easiest private way in from anywhere: your devices reach this one by name, encrypted, with nothing opened to the internet.'),
      ts.running ? h('dl', { class: 'facts' }, h('dt', {}, 'Name'), h('dd', {}, copyable(ts.dns || '')), h('dt', {}, 'Addresses'), h('dd', { class: 'mono' }, (ts.ips || []).join(', ')), ts.tailnet ? [h('dt', {}, 'Tailnet'), h('dd', {}, ts.tailnet)] : null) : null,
      h('div', { class: 'row' },
        !ts.installed ? h('button', { class: 'btn', onclick: () => runStep({ what: 'tool', agent: 'tailscale' }).then(loadNet) }, ic('download', 13), 'Install Tailscale') : null,
        ts.installed && !ts.running ? h('button', { class: 'btn primary', onclick: () => runStep({ what: 'tool-login', agent: 'tailscale' }).then(loadNet) }, ic('login', 13), 'Connect Tailscale') : null,
        ts.running ? h('button', { class: 'btn', title: 'tailscale serve: HTTPS at your MagicDNS name, inside your tailnet only', onclick: async () => {
          try {
            const r = await post('network/tailscale-serve', { on: true });
            toast('Serving over HTTPS in your tailnet');
            await dialog({ title: 'Tailscale HTTPS is on', text: `Other devices on your tailnet can open https://${ts.dns}/ now.`, body: r.output ? h('pre', { class: 'mono small' }, r.output) : null, ok: 'OK', cancel: null });
            loadNet();
          } catch (e) {
            toast(e.message, { err: true, ms: 9000 });
          }
        } }, ic('lock', 13), 'Serve over HTTPS (tailscale serve)') : null,
        ts.running ? h('button', { class: 'btn sm', onclick: () => post('network/tailscale-serve', { on: false }).then(() => (toast('Stopped'), loadNet())).catch((e) => toast(e.message, { err: true })) }, 'Stop serving') : null));
  }

  async function drawConns() {
    if (!S.me.local) return;
    let list = [];
    try {
      list = await get('connections');
    } catch { /* fine */ }
    connBox.replaceChildren(kicker('Connect to a server', h('small', { class: 'faint' }, 'use another machine’s agents and projects from this app')),
      list.length ? h('div', { class: 'conn-list' }, list.map((c) => h('div', { class: 'conn cut' }, ic('server', 16), h('div', {}, h('b', {}, c.name), h('small', { class: 'faint mono' }, c.url)), h('span', { class: 'tag' }, c.via),
        h('button', { class: 'btn sm primary', onclick: () => switchTo(c) }, 'Open'),
        h('button', { class: 'ibtn sm', title: 'Forget', onclick: async () => {
          await del('connections/' + c.id);
          drawConns();
        }, html: icon('trash', 14) })))) : h('p', { class: 'faint small' }, 'None yet.'),
      h('div', { class: 'row' },
        h('button', { class: 'btn', onclick: () => addConnection().then(drawConns) }, ic('plus', 13), 'Add a server'),
        h('button', { class: 'btn', onclick: () => discover(drawConns) }, ic('wifi', 13), 'Look on this network')));
  }

  guide.replaceChildren(kicker('Ways to reach a server'),
    h('div', { class: 'ways' },
      way('Same network', 'lan', 'Use the LAN address (192.168.x.x or the hostname). Look on this network finds servers for you.'),
      way('Tailscale', 'tailscale', 'Install Tailscale on both machines and sign in to the same account. Use the MagicDNS name; turn on “Serve over HTTPS” for https.'),
      way('WireGuard', 'wireguard', 'Bring the tunnel up on both ends, then use the server’s WireGuard address (often 10.x.x.x). Set “Listen on” to it to keep the server off other networks.'),
      way('Your domain', 'domain', 'Point the domain at the server through a reverse proxy (Caddy, nginx) or a tunnel (Cloudflare Tunnel) with HTTPS, then put it under Your domain. Don’t expose plain http to the internet.'),
      way('A hostname', 'host', 'Machine names work on most home networks (name or name.local).')));
  function way(title, tag, text) {
    return h('div', { class: 'way' }, h('span', { class: 'tag' }, tag), h('b', {}, title), h('p', { class: 'faint small' }, text));
  }

  await loadNet();
  drawPair(null);
  drawDevices();
  drawConns();
  const un = listen((ev) => ev.kind === 'devices' && drawDevices());
  const unw = watch((w) => w === 'settings' && drawShare());
  return () => (un(), unw(), clearInterval(pairTimer));
}

export async function addConnection(preset = {}) {
  const url = input(preset.url || '', { placeholder: 'roamer-box:4630, 100.101.2.3, https://roamer.example.com' });
  const code = input('', { placeholder: 'ABCD-EFGH', class: 'input code-in', autocomplete: 'off' });
  const name = input(preset.name || '', { placeholder: 'Optional: what to call it' });
  const status = h('div', { class: 'small' });
  url.onchange = async () => {
    if (!url.value.trim()) return;
    status.replaceChildren(h('span', { class: 'spin' }), ' Looking…');
    const r = await post('connections/test', { url: url.value.trim() }).catch((e) => ({ ok: false, error: e.message }));
    status.replaceChildren(r.ok ? h('span', { class: 'status acc' }, `Found ${r.server.name} (v${r.server.version})`) : h('span', { class: 'status hot' }, r.error));
    if (r.ok && !name.value) name.value = r.server.name;
  };
  if (preset.url) setTimeout(() => url.onchange());
  let made = null;
  const ok = await dialog({
    title: 'Connect to a server',
    text: 'On the server, open Settings › Remote access and make a pairing code (or run “model-roamer pair” there).',
    body: h('div', { class: 'dlg-fields' }, field('Address', url), status, field('Pairing code', code), field('Name', name)),
    ok: 'Pair',
    check: async () => {
      if (!url.value.trim() || !code.value.trim()) return 'The address and the code are both needed.';
      try {
        made = await post('connections', { url: url.value.trim(), code: code.value.trim(), name: name.value.trim() });
      } catch (e) {
        return e.message;
      }
    },
  });
  if (ok && made) {
    const open = await dialog({ title: `Paired with ${made.name}`, text: 'Open it now? You can switch back to this computer any time from the title bar.', ok: 'Open it', cancel: 'Later' });
    if (open) switchTo(made);
  }
}

async function discover(after) {
  const box = h('div', {}, loading('Listening for servers on this network (a few seconds)'));
  const p = dialog({ title: 'Servers on this network', body: box, ok: null, cancel: 'Close' });
  try {
    const found = await get('discover', { timeout: 15000 });
    const ok = found.filter((f) => !f.error);
    box.replaceChildren(ok.length ? h('div', { class: 'conn-list' }, ok.map((f) => h('div', { class: 'conn cut' }, ic('server', 16), h('div', {}, h('b', {}, f.name), h('small', { class: 'faint mono' }, f.url || f.addr)), h('button', { class: 'btn sm primary', onclick: (e) => {
      e.currentTarget.closest('.dlg-back')?.dispatchEvent(new PointerEvent('pointerdown'));
      addConnection({ url: f.url || f.addr, name: f.name }).then(after);
    } }, 'Pair')))) : h('p', { class: 'faint' }, found[0]?.error || 'None answered. Make sure sharing is on there and “discoverable” isn’t turned off, or add it by address.'));
  } catch (e) {
    box.replaceChildren(errorBox(e));
  }
  await p;
}

// ------------------------------------------------------------ about
async function about(el) {
  const upd = h('div', { class: 'upd' }, loading('Checking for updates'));
  const suite = h('div', { class: 'suite' });
  const logs = h('pre', { class: 'logpane', hidden: true });
  el.replaceChildren(
    h('section', { class: 'panel cut about' },
      h('div', { class: 'about-mark', html: `<svg viewBox="0 0 40 40" width="56" height="56">${(await import('../icons.js')).MARK}</svg>` }),
      h('div', {}, h('h2', {}, 'Model Roamer'), h('p', { class: 'faint' }, `Version ${S.version} · ${S.server.os} · ${S.server.headless ? 'server' : isDesktop ? 'desktop' : 'browser'}`), h('p', { class: 'small' }, 'An orchestration studio for Claude Code, Codex and Antigravity. Part of the Roamer suite.'))),
    h('section', { class: 'panel cut' }, kicker('Updates'), upd),
    h('section', { class: 'panel cut' }, kicker('The Roamer suite'), suite),
    h('section', { class: 'panel cut' }, kicker('Data and logs'),
      h('dl', { class: 'facts' }, h('dt', {}, 'Data folder'), h('dd', {}, copyable(S.dataDir))),
      h('div', { class: 'row' },
        S.me.local ? h('button', { class: 'btn sm', onclick: () => post('fs/reveal', { path: S.dataDir }) }, ic('folder', 13), 'Show the folder') : null,
        h('button', { class: 'btn sm', onclick: async () => {
          logs.hidden = !logs.hidden;
          if (!logs.hidden) {
            logs.textContent = 'Loading…';
            logs.textContent = (await get('logs').catch((e) => ({ text: e.message }))).text || '(empty)';
            logs.scrollTop = logs.scrollHeight;
          }
        } }, ic('terminal', 13), 'Show the log'),
        isDesktop ? h('button', { class: 'btn sm', onclick: () => native('devtools') }, ic('code', 13), 'Developer tools') : null),
      logs));

  async function check() {
    const u = await get('update').catch((e) => ({ error: e.message }));
    if (u.error) return upd.replaceChildren(h('p', { class: 'faint small' }, u.error), h('button', { class: 'btn sm', onclick: check }, 'Try again'));
    if (!u.newer) return upd.replaceChildren(h('p', {}, u.note || `You have the newest version (${u.current}).`), h('button', { class: 'btn sm', onclick: check }, 'Check again'));
    upd.replaceChildren(h('p', {}, h('b', {}, `${u.latest} is out`), ` (you have ${u.current}).`),
      u.notes ? h('details', {}, h('summary', {}, 'What’s new'), h('pre', { class: 'small' }, u.notes)) : null,
      u.can_self_update
        ? u.staged === u.latest
          ? h('button', { class: 'btn primary', onclick: apply }, 'Restart to update')
          : h('button', { class: 'btn primary', onclick: download }, ic('download', 13), 'Download the update')
        : h('button', { class: 'btn primary', onclick: () => openExternal(u.url) }, 'Get it from the release page'));
  }
  async function download() {
    upd.replaceChildren(h('div', { class: 'bar' }, h('i', { style: { width: '0%' } })), h('small', { class: 'faint' }, 'Downloading…'));
    await post('update', { action: 'download' }).catch((e) => toast(e.message, { err: true }));
  }
  async function apply() {
    const r = await post('update', { action: 'apply' }).catch((e) => ({ error: e.message }));
    if (!r.ok) toast(r.error || 'The update couldn’t be put in place. Try again, or download it from the release page.', { err: true });
    else upd.replaceChildren(h('p', {}, 'Restarting…'));
  }
  const un = listen((ev) => {
    if (ev.kind !== 'update') return;
    const d = ev.data;
    if (d.state === 'downloading') {
      const i = upd.querySelector('.bar i');
      if (i) i.style.width = d.pct + '%';
    } else if (d.state === 'ready') upd.replaceChildren(h('p', {}, `${d.tag} is ready.`), h('button', { class: 'btn primary', onclick: apply }, 'Restart to update'));
    else if (d.state === 'error') upd.replaceChildren(h('p', { class: 'hot' }, d.error), h('button', { class: 'btn sm', onclick: check }, 'Try again'));
  });
  check();
  get('suite').then((s) => suite.replaceChildren(...s.apps.map((a) => h('div', { class: 'suite-app cut' }, h('b', {}, a.name), h('small', { class: 'faint' }, a.about),
    h('div', { class: 'row' }, a.open ? h('button', { class: 'btn sm primary', onclick: () => openExternal(a.open) }, 'Open') : a.running === true && a.id === 'model-roamer' ? h('span', { class: 'status acc' }, 'You’re here') : null, h('button', { class: 'btn sm', onclick: () => openExternal(a.repo) }, 'GitHub')))))).catch(() => {});
  return un;
}

export { timeAgo, pickFolder, empty, select, go };
