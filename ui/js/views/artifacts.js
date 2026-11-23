// Artifacts: pages, documents, images and files the agents publish. They are served by whichever machine runs the
// work, so a phone or laptop connected to a server sees them too. HTML runs sandboxed in its own origin.

import { h, ago } from '../util.js';
import { icon } from '../icons.js';
import { get, post, patch, del, listen } from '../api.js';
import { S, agentName } from '../store.js';
import { go, openThread } from '../app.js';
import { popOut, setTitle, openExternal, isPop } from '../native.js';
import { dialog, toast, showMenu } from '../ui.js';
import { md, wireMd } from '../md.js';
import { ic, stamp, empty, loading, errorBox, field, input, textarea, select, kicker } from '../common.js';

const KIND_ICON = { html: 'globe', markdown: 'file', image: 'eye', svg: 'brush', text: 'file', pdf: 'file', file: 'download' };

export default async function artifacts(el, r, opts = {}) {
  const project = opts.project || r?.q?.project || '';
  const grid = h('div', { class: 'art-grid' });
  const q = input('', { placeholder: 'Filter artifacts', type: 'search' });
  if (!opts.project)
    el.append(h('div', { class: 'page-h' }, h('div', {}, h('h1', {}, 'Artifacts'), h('p', { class: 'lede' }, 'What the agents made for you to look at: pages, reports, diagrams, images. They open here, in a window of their own, or in your browser.')),
      h('button', { class: 'btn', onclick: () => newArtifact(project) }, ic('plus', 14), 'New')));
  el.append(h('div', { class: 'toolbar' }, q, opts.project ? h('button', { class: 'btn sm', onclick: () => newArtifact(project) }, ic('plus', 13), 'New') : null), grid);
  let all = [];
  async function load() {
    try {
      all = await get('artifacts' + (project ? '?project=' + project : ''));
      draw();
    } catch (e) {
      grid.replaceChildren(errorBox(e, load));
    }
  }
  function draw() {
    const f = q.value.trim().toLowerCase();
    const shown = all.filter((a) => !f || (a.title + ' ' + a.description + ' ' + (a.thread_title || '')).toLowerCase().includes(f));
    if (!all.length) return grid.replaceChildren(empty('artifact', 'Nothing published yet', 'When an agent makes something to look at (a mock-up, a report, a chart) it publishes it here with the publish_artifact tool. Ask for one in any thread.'));
    grid.replaceChildren(...shown.map((a) => h('a', { class: 'art-card cut', href: '#/a/' + a.id, oncontextmenu: (e) => (e.preventDefault(), menu(a, e.clientX, e.clientY, load)) },
      h('div', { class: 'ac-thumb k-' + a.kind }, a.kind === 'image' || a.kind === 'svg' ? h('img', { src: a.url, alt: '', loading: 'lazy' }) : a.kind === 'html' ? h('iframe', { src: a.url, loading: 'lazy', tabindex: '-1', sandbox: 'allow-scripts', scrolling: 'no' }) : h('i', { html: icon(KIND_ICON[a.kind] || 'file', 34) })),
      h('div', { class: 'ac-meta' }, h('b', {}, a.title), h('small', { class: 'faint' }, [a.kind, a.version > 1 ? 'v' + a.version : null, a.project_name, ago(a.updated_at)].filter(Boolean).join(' · ')), a.agent ? stamp(a.agent, { small: true }) : null))));
  }
  q.oninput = draw;
  grid.replaceChildren(loading());
  await load();
  return listen((ev) => (ev.kind === 'artifact' || ev.kind === 'resync') && load());
}

function menu(a, x, y, after) {
  showMenu([
    { label: 'Open', icon: 'eye', run: () => go('#/a/' + a.id) },
    { label: 'Open in its own window', icon: 'popout', run: () => popOut('#/a/' + a.id, { key: 'a:' + a.id, w: 1100, h: 820 }) },
    { label: 'Open in your browser', icon: 'external', run: () => openExternal(location.origin + a.url) },
    { label: 'Download', icon: 'download', run: () => window.open(a.url + '?download=1') },
    { sep: true },
    { label: 'Rename…', icon: 'edit', run: () => rename(a).then(after) },
    { label: 'Delete', icon: 'trash', danger: true, run: () => remove(a).then(after) },
  ], x, y);
}

async function rename(a) {
  const t = input(a.title);
  const d = textarea(a.description || '', { rows: 2 });
  if (!(await dialog({ title: 'Rename artifact', body: h('div', { class: 'dlg-fields' }, field('Title', t), field('Description', d)), ok: 'Save' }))) return;
  await patch('artifacts/' + a.id, { title: t.value, description: d.value });
}

async function remove(a) {
  await del('artifacts/' + a.id);
  toast(`Deleted ${a.title}`, { action: 'Undo', ms: 9000, run: () => patch('artifacts/' + a.id, { restore: true }) });
}

async function newArtifact(project) {
  const title = input('', { placeholder: 'Title' });
  let kind = 'markdown';
  const content = textarea('', { rows: 10, placeholder: 'Markdown, HTML or text' });
  const kindSel = select([['markdown', 'Markdown'], ['html', 'HTML page'], ['text', 'Text']], kind, (v) => (kind = v));
  let made = null;
  const ok = await dialog({
    title: 'New artifact',
    wide: true,
    body: h('div', { class: 'dlg-fields' }, field('Title', title), field('Kind', kindSel), field('Content', content)),
    ok: 'Publish',
    check: async () => {
      if (!title.value.trim()) return 'Give it a title.';
      try {
        made = await post('artifacts', { title: title.value.trim(), kind, content: content.value, project_id: project || null });
      } catch (e) {
        return e.message;
      }
    },
  });
  if (ok && made) go('#/a/' + made.id);
}

// ------------------------------------------------------------ viewer
export async function viewer(el, r, ctx) {
  let a;
  try {
    a = await get('artifacts/' + r.id);
  } catch (e) {
    return el.replaceChildren(empty('artifact', 'That artifact isn’t here', e.message, h('a', { class: 'btn', href: '#/artifacts' }, 'All artifacts')));
  }
  setTitle(`${a.title} · Artifact`);
  ctx?.setPopTitle?.(a.title);
  const frame = h('div', { class: 'av-frame' });
  const head = h('div', { class: 'av-head' });
  el.classList.add('artifact-view');
  el.append(head, frame);
  const drawHead = () => head.replaceChildren(
    isPop ? null : h('a', { class: 'ibtn', href: '#/artifacts', title: 'All artifacts', html: icon('back', 16) }),
    h('div', { class: 'av-title' }, h('h1', {}, a.title), h('small', { class: 'faint' }, [a.kind, 'v' + a.version, a.thread_title ? 'from ' + a.thread_title : null, 'updated ' + ago(a.updated_at)].filter(Boolean).join(' · '))),
    a.agent ? stamp(a.agent, { small: true }) : null,
    h('span', { class: 'grow' }),
    h('button', { class: 'btn sm', onclick: () => draw(true) }, ic('reload', 13), 'Reload'),
    a.thread_id ? h('button', { class: 'btn sm', onclick: () => openThread(a.thread_id) }, ic('thread', 13), 'Thread') : null,
    isPop ? null : h('button', { class: 'ibtn', title: 'Open in its own window', onclick: () => popOut('#/a/' + a.id, { key: 'a:' + a.id, w: 1100, h: 820 }), html: icon('popout', 16) }),
    h('button', { class: 'ibtn', title: 'Open in your browser', onclick: () => openExternal(location.origin + a.url), html: icon('external', 16) }),
    h('button', { class: 'ibtn', title: 'More', onclick: (e) => menu(a, e.clientX, e.clientY, async () => ((a = await get('artifacts/' + a.id)), drawHead())), html: icon('more', 16) }));
  async function draw(bust) {
    const src = a.url + (bust ? '?t=' + Date.now() : '');
    if (a.kind === 'markdown' || a.kind === 'text') {
      frame.replaceChildren(loading());
      try {
        const text = await (await fetch(src, { credentials: 'same-origin' })).text();
        frame.replaceChildren(a.kind === 'markdown' ? wireMd(h('div', { class: 'md doc', html: md(text) }), openExternal) : h('pre', { class: 'doc mono' }, text));
      } catch (e) {
        frame.replaceChildren(errorBox(e));
      }
    } else if (a.kind === 'image') frame.replaceChildren(h('div', { class: 'av-img' }, h('img', { src, alt: a.title })));
    else if (a.kind === 'file') frame.replaceChildren(empty('download', a.entry, a.description || 'A file to download.', h('a', { class: 'btn primary', href: a.url + '?download=1' }, 'Download')));
    else frame.replaceChildren(h('iframe', { class: 'av-if', src, sandbox: 'allow-scripts allow-forms allow-popups allow-modals allow-downloads', title: a.title }));
  }
  drawHead();
  draw();
  return listen((ev) => {
    if (ev.kind === 'artifact' && ev.data?.id === a.id) {
      if (ev.data.deleted) return frame.replaceChildren(empty('trash', 'This artifact was deleted'));
      a = { ...a, ...ev.data, url: a.url };
      drawHead();
      draw(true);
    }
  });
}

export { S, agentName, kicker };
