// A small, safe Markdown renderer for agent replies, memory and artifacts. Everything is escaped first; only the
// constructs below become HTML, and links only go to http(s)/mailto (they open in the person's browser).

const esc = (s) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');

function inline(s) {
  // code spans first, held aside so nothing inside them is touched
  const held = [];
  s = s.replace(/`([^`\n]+)`/g, (_, c) => (held.push(`<code>${c}</code>`), `\u0000${held.length - 1}\u0000`));
  s = s.replace(/!\[([^\]]*)\]\((https?:\/\/[^\s)]+)\)/g, (_, alt, url) => (held.push(`<img src="${url}" alt="${alt}" loading="lazy" referrerpolicy="no-referrer">`), `\u0000${held.length - 1}\u0000`));
  s = s.replace(/\[([^\]]+)\]\(((?:https?:\/\/|mailto:|#\/)[^\s)]+)\)/g, (_, t, url) => (held.push(`<a href="${url}" data-ext="${url.startsWith('#') ? '' : '1'}">${t}</a>`), `\u0000${held.length - 1}\u0000`));
  s = s.replace(/(^|[\s(])(https?:\/\/[^\s<)]+[^\s<).,;:!?'"])/g, (_, pre, url) => (held.push(`<a href="${url}" data-ext="1">${url}</a>`), `${pre}\u0000${held.length - 1}\u0000`));
  s = s.replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>').replace(/__([^_]+)__/g, '<strong>$1</strong>');
  s = s.replace(/(^|[^*\w])\*([^*\n]+)\*(?!\w)/g, '$1<em>$2</em>').replace(/(^|[^_\w])_([^_\n]+)_(?!\w)/g, '$1<em>$2</em>');
  s = s.replace(/~~([^~]+)~~/g, '<del>$1</del>');
  return s.replace(/\u0000(\d+)\u0000/g, (_, i) => held[+i]);
}

function table(rows) {
  const cells = (r) => r.replace(/^\s*\|/, '').replace(/\|\s*$/, '').split('|').map((c) => c.trim());
  const head = cells(rows[0]);
  const body = rows.slice(2).map(cells);
  return `<div class="md-table"><table><thead><tr>${head.map((c) => `<th>${inline(c)}</th>`).join('')}</tr></thead><tbody>${body
    .map((r) => `<tr>${r.map((c) => `<td>${inline(c)}</td>`).join('')}</tr>`)
    .join('')}</tbody></table></div>`;
}

export function md(src) {
  const lines = esc(String(src ?? '')).replace(/\r\n?/g, '\n').split('\n');
  const out = [];
  let i = 0;
  while (i < lines.length) {
    const l = lines[i];
    const fence = l.match(/^\s*(```|~~~)\s*([\w+-]*)/);
    if (fence) {
      const buf = [];
      i++;
      while (i < lines.length && !lines[i].trim().startsWith(fence[1])) buf.push(lines[i++]);
      i++;
      out.push(`<div class="md-code"><div class="md-code-h"><span>${fence[2] || 'text'}</span><button class="md-copy" type="button">copy</button></div><pre><code>${buf.join('\n')}</code></pre></div>`);
      continue;
    }
    if (/^\s*$/.test(l)) {
      i++;
      continue;
    }
    const hd = l.match(/^(#{1,6})\s+(.*)$/);
    if (hd) {
      const n = Math.min(6, hd[1].length + 1);
      out.push(`<h${n}>${inline(hd[2])}</h${n}>`);
      i++;
      continue;
    }
    if (/^\s*([-*_])(\s*\1){2,}\s*$/.test(l)) {
      out.push('<hr>');
      i++;
      continue;
    }
    if (/^\s*\|.*\|\s*$/.test(l) && /^\s*\|?\s*:?-{2,}/.test(lines[i + 1] || '')) {
      const rows = [];
      while (i < lines.length && /^\s*\|.*\|\s*$/.test(lines[i])) rows.push(lines[i++]);
      out.push(table(rows));
      continue;
    }
    if (/^\s*&gt;/.test(l)) {
      const buf = [];
      while (i < lines.length && /^\s*&gt;/.test(lines[i])) buf.push(lines[i++].replace(/^\s*&gt;\s?/, ''));
      out.push(`<blockquote>${md(buf.join('\n').replace(/&gt;/g, '>').replace(/&lt;/g, '<').replace(/&amp;/g, '&'))}</blockquote>`);
      continue;
    }
    const li = l.match(/^(\s*)([-*+]|\d+[.)])\s+(.*)$/);
    if (li) {
      const ordered = /\d/.test(li[2]);
      const items = [];
      while (i < lines.length) {
        const m = lines[i].match(/^(\s*)([-*+]|\d+[.)])\s+(.*)$/);
        if (m) {
          const task = m[3].match(/^\[([ xX])\]\s+(.*)$/);
          items.push({ depth: Math.floor(m[1].replace(/\t/g, '  ').length / 2), text: task ? `<i class="md-task${task[1] !== ' ' ? ' on' : ''}"></i>${inline(task[2])}` : inline(m[3]) });
          i++;
        } else if (/^\s{2,}\S/.test(lines[i]) && items.length) {
          items[items.length - 1].text += ' ' + inline(lines[i].trim());
          i++;
        } else break;
      }
      let html = '', depth = -1;
      const tag = ordered ? 'ol' : 'ul';
      for (const it of items) {
        while (depth < it.depth) (html += `<${tag}>`), depth++;
        while (depth > it.depth) (html += `</${tag}>`), depth--;
        html += `<li>${it.text}</li>`;
      }
      while (depth-- >= 0) html += `</${tag}>`;
      out.push(html);
      continue;
    }
    const buf = [];
    while (i < lines.length && lines[i].trim() && !/^(#{1,6}\s|\s*```|\s*~~~|\s*&gt;|\s*([-*+]|\d+[.)])\s+)/.test(lines[i])) buf.push(lines[i++]);
    if (!buf.length) buf.push(lines[i++]);
    out.push(`<p>${buf.map(inline).join('<br>')}</p>`);
  }
  return out.join('');
}

/** Wire up a rendered block: copy buttons on code, external links to the browser. */
export function wireMd(el, openExternal) {
  el.addEventListener('click', (e) => {
    const c = e.target.closest('.md-copy');
    if (c) {
      const code = c.closest('.md-code').querySelector('code').textContent;
      navigator.clipboard?.writeText(code).catch(() => {});
      c.textContent = 'copied';
      setTimeout(() => (c.textContent = 'copy'), 1200);
      return;
    }
    const a = e.target.closest('a[data-ext="1"]');
    if (a) {
      e.preventDefault();
      openExternal(a.getAttribute('href'));
    }
  });
  return el;
}
