// Artifacts: pages, documents, images and files the agents publish. They are served by whichever machine runs the
// work, so a phone or laptop connected to a server sees them too. HTML runs sandboxed in its own origin.

import { h, ago } from '../util.js';
import { icon } from '../icons.js';
import { get, post, patch, del, listen } from '../api.js';
import { S, agentName } from '../store.js';
import { go, back, openThread } from '../app.js';
import { popOut, setTitle, openExternal, openOutside, isPop } from '../native.js';
import { dialog, toast, showMenu } from '../ui.js';
import { md, wireMd } from '../md.js';
import { ic, stamp, empty, loading, errorBox, field, input, textarea, select, kicker, askLine } from '../common.js';

const KIND_ICON = { html: 'globe', markdown: 'file', image: 'eye', svg: 'brush', text: 'file', pdf: 'file', file: 'download' };

const OPEN_KEY = 'mr-art-open';
const DND_ART = 'application/x-roamer-artifact';
const DND_FLD = 'application/x-roamer-folder';

/** The library: a folder tree on the left (projects, their threads, chats, and folders anyone made), the open folder
 * on the right with breadcrumbs. Drag artifacts and folders onto a folder, a crumb or the tree to move them. */
export default async function artifacts(el, r, opts = {}) {
  const project = opts.project || r?.q?.project || '';
  let folders = {}, arts = [], root = null, cur = r?.q?.f || null;
  let open;
  try {
    open = new Set(JSON.parse(localStorage.getItem(OPEN_KEY) || '[]'));
  } catch {
    open = new Set();
  }
  const saveOpen = () => {
    try {
      localStorage.setItem(OPEN_KEY, JSON.stringify([...open].slice(-400)));
    } catch { /* private window */ }
  };
  const tree = h('nav', { class: 'art-tree' });
  const crumbs = h('div', { class: 'art-crumbs' });
  const grid = h('div', { class: 'art-grid' });
  const q = input('', { placeholder: 'Filter artifacts', type: 'search' });
  if (!opts.project)
    el.append(h('div', { class: 'page-h' }, h('div', {}, h('h1', {}, 'Artifacts'), h('p', { class: 'lede' }, 'What the agents made for you to look at: pages, reports, diagrams, images. Each project and chat files its own; make folders of your own and drag things where you want them.')),
      h('button', { class: 'btn', onclick: () => newArtifact(project, cur) }, ic('plus', 14), 'New')));
  el.append(h('div', { class: 'art-layout' + (opts.project ? ' in-project' : '') }, tree,
    h('section', { class: 'art-main' },
      h('div', { class: 'toolbar' }, crumbs, h('span', { class: 'grow' }), q,
        h('button', { class: 'btn sm', onclick: () => newFolder(cur) }, ic('folder', 13), 'New folder'),
        opts.project ? h('button', { class: 'btn sm', onclick: () => newArtifact(project, cur) }, ic('plus', 13), 'New') : null),
      grid)));

  const kids = (id) => Object.values(folders).filter((f) => (f.parent_id || null) === id && (id !== null || !root || f.id === root));
  const chain = (id) => {
    const out = [];
    for (let c = id, n = 0; c && folders[c] && n < 40; c = folders[c].parent_id, n++) {
      out.unshift(folders[c]);
      if (c === root) break;
    }
    return out;
  };
  const inside = (id) => {
    const seen = new Set([id]);
    let grew = true;
    while (grew) {
      grew = false;
      for (const f of Object.values(folders)) if (f.parent_id && seen.has(f.parent_id) && !seen.has(f.id)) (seen.add(f.id), (grew = true));
    }
    return seen;
  };

  async function load() {
    try {
      const [fl, list] = await Promise.all([get('folders' + (project ? '?project=' + project : '')), get('artifacts' + (project ? '?project=' + project : ''))]);
      folders = Object.fromEntries(fl.folders.map((f) => [f.id, f]));
      root = fl.root || null;
      arts = list;
      if (cur && !folders[cur]) cur = null;
      if (!cur && root) cur = root;
      draw();
    } catch (e) {
      grid.replaceChildren(errorBox(e, load));
    }
  }

  // ---------------------------------------------------------- moving things
  async function moveInto(target, e) {
    const art = e.dataTransfer.getData(DND_ART), fld = e.dataTransfer.getData(DND_FLD);
    try {
      if (art) {
        const a = arts.find((x) => x.id === art);
        if (!a || (a.folder_id || null) === target) return;
        await patch('artifacts/' + art, { folder_id: target || '' });
        toast(`Moved ${a.title}`, { action: 'Undo', run: () => patch('artifacts/' + art, { folder_id: a.folder_id || '' }).then(load) });
      } else if (fld) {
        const f = folders[fld];
        if (!f || fld === target || (f.parent_id || null) === target) return;
        if (target && inside(fld).has(target)) return toast('A folder can’t go inside itself.', { err: true });
        await patch('folders/' + fld, { parent_id: target || '' });
        if (target) open.add(target), saveOpen();
        toast(`Moved ${f.name}`, { action: 'Undo', run: () => patch('folders/' + fld, { parent_id: f.parent_id || '' }).then(load) });
      }
      await load();
    } catch (err) {
      toast(err.message, { err: true });
    }
  }
  /** Props that make an element a place to drop artifacts and folders. */
  const dropTo = (target) => ({
    ondragover: (e) => {
      if (![...e.dataTransfer.types].some((t) => t === DND_ART || t === DND_FLD)) return;
      e.preventDefault();
      e.dataTransfer.dropEffect = 'move';
      e.currentTarget.classList.add('drop');
    },
    ondragleave: (e) => e.currentTarget.classList.remove('drop'),
    ondrop: (e) => {
      e.preventDefault();
      e.currentTarget.classList.remove('drop');
      moveInto(target, e);
    },
  });
  const dragOf = (type, id) => ({ draggable: 'true', ondragstart: (e) => (e.dataTransfer.setData(type, id), (e.dataTransfer.effectAllowed = 'move'), e.currentTarget.classList.add('dragging')), ondragend: (e) => e.currentTarget.classList.remove('dragging') });

  // ---------------------------------------------------------- drawing
  function openFolder(id) {
    cur = id;
    if (id) for (const f of chain(id)) if (f.id !== id) open.add(f.id);
    saveOpen();
    // the folder you're in is part of the address (without a history entry of its own), so Back from an artifact
    // returns to this very folder
    if (!opts.project) {
      try {
        history.replaceState(history.state, '', '#/artifacts' + (id && id !== root ? '?f=' + id : ''));
      } catch { /* fine */ }
    }
    q.value = '';
    draw();
  }
  function node(f, depth) {
    const ch = kids(f.id);
    const isOpen = open.has(f.id);
    const row = h('div', { class: 'at-row' + (cur === f.id ? ' on' : ''), style: { '--d': depth }, ...dragOf(DND_FLD, f.id), ...dropTo(f.id),
      onclick: () => openFolder(f.id), oncontextmenu: (e) => (e.preventDefault(), folderMenu(f, e.clientX, e.clientY)) },
      ch.length ? h('button', { class: 'at-tw' + (isOpen ? ' open' : ''), type: 'button', title: isOpen ? 'Collapse' : 'Expand', onclick: (e) => {
        e.stopPropagation();
        isOpen ? open.delete(f.id) : open.add(f.id);
        saveOpen();
        drawTree();
      }, html: icon('right', 11) }) : h('i', { class: 'at-tw' }),
      ic(f.auto_key?.startsWith('project:') ? 'board' : f.auto_key?.startsWith('thread:') ? 'thread' : f.auto_key === 'chats' ? 'chat' : 'folder', 14),
      h('span', { class: 'at-name' }, f.name),
      f.count ? h('small', {}, String(f.count)) : null);
    return [row, ...(isOpen ? ch.flatMap((c) => node(c, depth + 1)) : [])];
  }
  function drawTree() {
    const top = root ? [folders[root]].filter(Boolean) : kids(null);
    const unfiled = arts.filter((a) => !a.folder_id || !folders[a.folder_id]).length;
    tree.replaceChildren(
      root ? null : h('div', { class: 'at-row top' + (cur === null ? ' on' : ''), ...dropTo(null), onclick: () => openFolder(null) }, h('i', { class: 'at-tw' }), ic('layers', 14), h('span', { class: 'at-name' }, 'All artifacts'), unfiled ? h('small', {}, String(unfiled)) : null),
      ...top.flatMap((f) => node(f, root ? 0 : 1)));
  }
  function drawCrumbs() {
    const parts = chain(cur);
    crumbs.replaceChildren(
      ...(root ? [] : [h('button', { class: 'crumb', type: 'button', ...dropTo(null), onclick: () => openFolder(null) }, 'All')]),
      ...parts.flatMap((f, i) => [
        root && i === 0 ? null : h('i', { class: 'crumb-sep' }, '/'),
        h('button', { class: 'crumb' + (f.id === cur ? ' on' : ''), type: 'button', ...dropTo(f.id), onclick: () => openFolder(f.id), oncontextmenu: (e) => (e.preventDefault(), folderMenu(f, e.clientX, e.clientY)) }, f.name),
      ]));
  }
  function folderCard(f) {
    const n = (f.count || 0) + kids(f.id).length;
    return h('button', { class: 'fld-card cut', type: 'button', ...dragOf(DND_FLD, f.id), ...dropTo(f.id), onclick: () => openFolder(f.id), oncontextmenu: (e) => (e.preventDefault(), folderMenu(f, e.clientX, e.clientY)) },
      ic(f.auto_key?.startsWith('thread:') ? 'thread' : f.auto_key?.startsWith('project:') ? 'board' : f.auto_key === 'chats' ? 'chat' : 'folder', 20),
      h('b', {}, f.name), h('small', { class: 'faint' }, n ? `${n} item${n === 1 ? '' : 's'}` : 'Empty'));
  }
  function artCard(a) {
    return h('a', { class: 'art-card cut', href: '#/a/' + a.id, ...dragOf(DND_ART, a.id), oncontextmenu: (e) => (e.preventDefault(), menu(a, e.clientX, e.clientY, load, { move: () => moveDialog(a) })) },
      h('div', { class: 'ac-thumb k-' + a.kind }, a.kind === 'image' || a.kind === 'svg' ? h('img', { src: a.url, alt: '', loading: 'lazy', draggable: 'false' }) : a.kind === 'html' ? h('iframe', { src: a.url, loading: 'lazy', tabindex: '-1', sandbox: 'allow-scripts', scrolling: 'no' }) : h('i', { html: icon(KIND_ICON[a.kind] || 'file', 34) })),
      h('div', { class: 'ac-meta' }, h('b', {}, a.title), h('small', { class: 'faint' }, [a.kind, a.version > 1 ? 'v' + a.version : null, a.project_name, ago(a.updated_at)].filter(Boolean).join(' · ')), a.agent ? stamp(a.agent, { small: true }) : null));
  }
  function draw() {
    drawTree();
    drawCrumbs();
    const f = q.value.trim().toLowerCase();
    if (f) {
      const hits = arts.filter((a) => (a.title + ' ' + a.description + ' ' + (a.thread_title || '')).toLowerCase().includes(f));
      return grid.replaceChildren(...(hits.length ? hits.map(artCard) : [empty('search', 'Nothing matches', 'Try another word. The filter looks through every folder.')]));
    }
    if (!arts.length && !Object.keys(folders).length) return grid.replaceChildren(empty('artifact', 'Nothing published yet', 'When an agent makes something to look at (a mock-up, a report, a chart) it publishes it here with the publish_artifact tool, filed under its project and thread. Ask for one in any thread.'));
    const subs = cur === null ? kids(null) : kids(cur);
    const here = arts.filter((a) => (cur === null ? !a.folder_id || !folders[a.folder_id] : a.folder_id === cur));
    if (!subs.length && !here.length) return grid.replaceChildren(h('div', { class: 'fld-empty', ...dropTo(cur) }, empty('folder', 'This folder is empty', 'Drag artifacts or folders here, or make a folder inside it.', h('button', { class: 'btn sm', onclick: () => newFolder(cur) }, ic('folder', 13), 'New folder'))));
    grid.replaceChildren(...subs.map(folderCard), ...here.map(artCard));
  }

  // ---------------------------------------------------------- folder actions
  async function newFolder(parent) {
    const name = await askLine('New folder', '', 'Name', { ok: 'Make it', placeholder: 'e.g. Designs' });
    if (!name?.trim()) return;
    try {
      const f = await post('folders', { name: name.trim(), parent_id: parent || '' });
      if (parent) open.add(parent), saveOpen();
      await load();
      return f;
    } catch (e) {
      toast(e.message, { err: true });
    }
  }
  async function renameFolder(f) {
    const name = await askLine('Rename folder', f.name, 'Name', { ok: 'Rename' });
    if (!name?.trim() || name.trim() === f.name) return;
    await patch('folders/' + f.id, { name: name.trim() }).catch((e) => toast(e.message, { err: true }));
    load();
  }
  async function removeFolder(f) {
    const r = await del('folders/' + f.id).catch((e) => (toast(e.message, { err: true }), null));
    if (!r) return;
    if (cur && inside(f.id).has(cur)) cur = f.parent_id || root || null;
    await load();
    toast(`Deleted ${f.name}${r.artifacts ? ` and ${r.artifacts} artifact${r.artifacts === 1 ? '' : 's'} in it` : ''}`, { action: 'Undo', ms: 9000, run: () => patch('folders/' + f.id, { restore: true }).then(load) });
  }
  function folderMenu(f, x, y) {
    showMenu([
      { label: 'Open', icon: 'folder', run: () => openFolder(f.id) },
      { label: 'New folder inside…', icon: 'plus', run: () => newFolder(f.id) },
      { label: 'Rename…', icon: 'edit', run: () => renameFolder(f) },
      f.parent_id && f.id !== root ? { label: 'Move to the top', icon: 'up', run: () => patch('folders/' + f.id, { parent_id: root || '' }).then(load) } : null,
      { sep: true },
      { label: 'Delete', icon: 'trash', danger: true, run: () => removeFolder(f) },
    ].filter(Boolean), x, y);
  }
  async function moveDialog(a) {
    const opts = [['', root ? folders[root]?.name || 'Top' : 'Not in a folder'], ...Object.values(folders).filter((f) => f.id !== root).map((f) => [f.id, chain(f.id).map((x) => x.name).join(' / ')]).sort((x, y) => x[1].localeCompare(y[1]))];
    let pick = a.folder_id || '';
    if (!(await dialog({ title: `Move ${a.title}`, body: h('div', { class: 'dlg-fields' }, field('Folder', select(opts, pick, (v) => (pick = v)))), ok: 'Move' }))) return;
    await patch('artifacts/' + a.id, { folder_id: pick || root || '' }).catch((e) => toast(e.message, { err: true }));
    load();
  }

  q.oninput = draw;
  grid.replaceChildren(loading());
  await load();
  return listen((ev) => (ev.kind === 'artifact' || ev.kind === 'resync') && load());
}

function menu(a, x, y, after, extra = {}) {
  showMenu([
    { label: 'Open', icon: 'eye', run: () => go('#/a/' + a.id) },
    { label: 'Open in its own window', icon: 'popout', run: () => popOut('#/a/' + a.id, { key: 'a:' + a.id, w: 1100, h: 820 }) },
    { label: 'Open in your browser', icon: 'external', run: () => openOutside(location.origin + a.url) },
    { label: 'Download', icon: 'download', run: () => window.open(a.url + '?download=1') },
    { sep: true },
    { label: 'Rename…', icon: 'edit', run: () => rename(a).then(after) },
    extra.move ? { label: 'Move to folder…', icon: 'folder', run: extra.move } : null,
    { label: 'Delete', icon: 'trash', danger: true, run: () => remove(a).then(after) },
  ].filter(Boolean), x, y);
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

async function newArtifact(project, folder) {
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
        made = await post('artifacts', { title: title.value.trim(), kind, content: content.value, project_id: project || null, folder_id: folder || '' });
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
  const cbox = h('div', { class: 'av-comment', hidden: true });
  el.classList.add('artifact-view');
  el.append(head, frame, cbox);
  // ---- comments: point at a part (a page element, selected text, a spot on an image), say something, and send it to
  // the thread that made the artifact or to the project's orchestrator
  let commenting = false;
  const pickable = ['html', 'markdown', 'text', 'image', 'svg'].includes(a.kind);
  const getFrame = () => frame.querySelector('iframe.av-if');
  const setMode = (on) => {
    commenting = on;
    frame.classList.toggle('commenting', on);
    getFrame()?.contentWindow?.postMessage({ mr: 'mode', on }, '*');
    drawHead();
  };
  function openComment(where, quote) {
    const dests = [a.thread_id ? ['thread', 'The thread' + (a.thread_title ? ': ' + a.thread_title : '')] : null, a.project_id ? ['lead', 'The orchestrator'] : null].filter(Boolean);
    if (!dests.length) return toast('This artifact has no thread or project to send a comment to.', { err: true });
    const saved = localStorage.getItem('mr-art-dest');
    let dest = dests.some(([v]) => v === saved) ? saved : dests[0][0];
    const ta = textarea('', { rows: 3, placeholder: 'What should change, or what do you want to know?' });
    const send = h('button', { class: 'btn primary sm' }, 'Send');
    const close = () => ((cbox.hidden = true), cbox.replaceChildren());
    send.onclick = async () => {
      if (!ta.value.trim()) return ta.focus();
      send.disabled = true;
      try {
        let tid = a.thread_id;
        if (dest === 'lead') tid = (await get('projects/' + a.project_id)).lead.id;
        const quoted = quote ? '\n> ' + quote.replace(/\n/g, '\n> ') + '\n' : '';
        const text = `Comment on the artifact "${a.title}" (${a.kind}, v${a.version}, id ${a.id}).\n${where}\n${quoted}\n${ta.value.trim()}`;
        await post(`threads/${tid}/messages`, { text });
        localStorage.setItem('mr-art-dest', dest);
        toast(dest === 'lead' ? 'Sent to the orchestrator' : 'Sent to the thread');
        close();
      } catch (e) {
        toast(e.message, { err: true });
        send.disabled = false;
      }
    };
    cbox.replaceChildren(
      h('div', { class: 'avc-what mono' }, ic('chat', 13), h('span', {}, where)),
      quote ? h('blockquote', { class: 'avc-quote' }, quote.length > 280 ? quote.slice(0, 280) + '…' : quote) : null,
      ta,
      h('div', { class: 'avc-row' }, h('span', { class: 'faint small' }, 'Send to'), select(dests, dest, (v) => (dest = v)), h('span', { class: 'grow' }), h('button', { class: 'btn sm', onclick: close }, 'Cancel'), send));
    cbox.hidden = false;
    ta.focus();
  }
  const onMsg = (e) => {
    const f = getFrame();
    if (!f || e.source !== f.contentWindow || e.data?.mr !== 'pick') return;
    const d = e.data;
    openComment(`Part of the page: <${String(d.tag || '').slice(0, 20)}> at \`${String(d.selector || '').slice(0, 240)}\` (about ${+d.x || 0}% across, ${+d.y || 0}% down the view)`, String(d.text || '').slice(0, 300));
  };
  addEventListener('message', onMsg);
  const drawHead = () => head.replaceChildren(
    isPop ? null : h('button', { class: 'ibtn', title: 'Back', onclick: () => back('#/artifacts'), html: icon('back', 16) }),
    h('div', { class: 'av-title' }, h('h1', {}, a.title), h('small', { class: 'faint' }, [a.kind, 'v' + a.version, a.thread_title ? 'from ' + a.thread_title : null, 'updated ' + ago(a.updated_at)].filter(Boolean).join(' · '))),
    a.agent ? stamp(a.agent, { small: true }) : null,
    h('span', { class: 'grow' }),
    pickable ? h('button', { class: 'btn sm' + (commenting ? ' primary' : ''), title: a.kind === 'html' || a.kind === 'image' || a.kind === 'svg' ? 'Click a part of it to comment' : 'Select text to comment', onclick: () => setMode(!commenting) }, ic('chat', 13), commenting ? 'Commenting…' : 'Comment') : null,
    h('button', { class: 'btn sm', onclick: () => draw(true) }, ic('reload', 13), 'Reload'),
    a.thread_id ? h('button', { class: 'btn sm', onclick: () => openThread(a.thread_id) }, ic('thread', 13), 'Thread') : null,
    isPop ? null : h('button', { class: 'ibtn', title: 'Open in its own window', onclick: () => popOut('#/a/' + a.id, { key: 'a:' + a.id, w: 1100, h: 820 }), html: icon('popout', 16) }),
    h('button', { class: 'ibtn', title: 'Open in your browser', onclick: () => openOutside(location.origin + a.url), html: icon('external', 16) }),
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
    else {
      const f = h('iframe', { class: 'av-if', src: a.url + '?annotate=1' + (bust ? '&t=' + Date.now() : ''), sandbox: 'allow-scripts allow-forms allow-popups allow-modals allow-downloads', title: a.title });
      f.onload = () => commenting && f.contentWindow.postMessage({ mr: 'mode', on: true }, '*');
      frame.replaceChildren(f);
    }
  }
  // text documents and images: select words, or click a spot
  frame.addEventListener('mouseup', () => {
    if (!commenting || (a.kind !== 'markdown' && a.kind !== 'text')) return;
    const q = String(getSelection() || '').trim();
    if (q.length > 1) openComment('Part of the text:', q);
  });
  frame.addEventListener('click', (e) => {
    if (!commenting || (a.kind !== 'image' && a.kind !== 'svg')) return;
    const im = frame.querySelector('img');
    if (!im || e.target !== im) return;
    const r = im.getBoundingClientRect();
    openComment(`A spot on the image: ${Math.round(((e.clientX - r.left) / r.width) * 100)}% across, ${Math.round(((e.clientY - r.top) / r.height) * 100)}% down.`, '');
  });
  drawHead();
  draw();
  const off = listen((ev) => {
    if (ev.kind === 'artifact' && ev.data?.id === a.id) {
      if (ev.data.deleted) return frame.replaceChildren(empty('trash', 'This artifact was deleted'));
      a = { ...a, ...ev.data, url: a.url };
      drawHead();
      draw(true);
    }
  });
  return () => {
    removeEventListener('message', onMsg);
    off?.();
  };
}

export { S, agentName, kicker };
