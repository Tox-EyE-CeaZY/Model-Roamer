// The first run: what Model Roamer is, which agents this machine has, and the first project.

import { h } from '../util.js';
import { S, watch, setSetting, agentList } from '../store.js';
import { go, newProject } from '../app.js';
import { runStep } from '../term.js';
import { get } from '../api.js';
import { ic, stamp } from '../common.js';
import { MARK } from '../icons.js';

export default async function welcome(el) {
  const fleet = h('div', { class: 'wl-fleet' });
  const draw = () => fleet.replaceChildren(...agentList().map((a) => {
    const ready = a.installed && a.auth?.signed_in === 'yes';
    return h('div', { class: 'wl-agent cut', style: { '--ah': a.info?.hue } },
      h('i', { class: 'ag-sw' }), h('div', {}, h('b', {}, a.info?.name), h('small', { class: 'faint' }, a.info?.vendor)),
      a.pending ? h('span', { class: 'status dim' }, h('span', { class: 'spin' }), 'Checking')
        : ready ? h('span', { class: 'status acc' }, 'Ready')
          : !a.installed ? h('button', { class: 'btn sm', onclick: () => runStep({ what: 'install', agent: a.id, title: a.install?.[0]?.label }) }, 'Install')
            : h('button', { class: 'btn sm primary', onclick: () => runStep({ what: 'login', agent: a.id, title: a.login?.label }) }, 'Sign in'));
  }));
  draw();
  if (agentList().some((a) => a.pending)) get('agents?refresh=1', { timeout: 90000 }).catch(() => {});
  const done = () => setSetting({ welcomed: true });
  el.append(h('div', { class: 'welcome' },
    h('div', { class: 'wl-hero' },
      h('div', { class: 'about-mark', html: `<svg viewBox="0 0 40 40" width="64" height="64">${MARK}</svg>` }),
      h('div', { class: 'eyebrow' }, 'Welcome to'),
      h('h1', { class: 'wl-title' }, 'MODEL', h('i', {}, '/'), 'ROAMER'),
      h('p', { class: 'lede' }, 'One lead agent plans the work and hands pieces to the others: Claude Code, Codex and Antigravity, each in its own branch so nobody steps on anybody. You watch, steer and merge.')),
    h('ol', { class: 'wl-steps' },
      h('li', {}, h('h3', {}, 'Get the agents ready'), h('p', { class: 'faint' }, 'Install and sign in to the ones you pay for. Model Roamer uses your own subscriptions through their CLIs. You need at least one.'), fleet),
      h('li', {}, h('h3', {}, 'Make a project'), h('p', { class: 'faint' }, 'A folder or GitHub repository plus a lead. Tell the lead what you want; it opens threads for the right agents.'),
        h('div', { class: 'row' }, h('button', { class: 'btn primary', onclick: async () => (await newProject()) && done() }, ic('plus', 14), 'New project'), h('button', { class: 'btn', onclick: () => (done(), go('#/chats')) }, 'Just chat for now'))),
      h('li', {}, h('h3', {}, 'Working from elsewhere?'), h('p', { class: 'faint' }, 'Run Model Roamer on an always-on machine (model-roamer server) and pair this app or your phone with it. Settings › Remote access.'),
        h('button', { class: 'btn', onclick: () => (done(), go('#/settings/remote')) }, ic('wifi', 14), 'Remote access'))),
    h('button', { class: 'linkbtn', onclick: () => (done(), go('#/')) }, 'Skip, take me to the app')));
  return watch((w) => w === 'agents' && draw());
}

export { stamp, S };
