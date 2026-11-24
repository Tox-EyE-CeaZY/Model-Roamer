// Reusable horizontal scroll strip affordance (tabs and other narrow horizontal strips):
// hides the native scrollbar, signals clipped tabs with directional edge fades, and renders
// a slim Roamer progress thumb only when content overflows horizontally.

import { h, clamp } from './util.js';

function prefersReducedMotion() {
  return document.documentElement.classList.contains('no-motion') ||
    (typeof matchMedia === 'function' && matchMedia('(prefers-reduced-motion: reduce)').matches);
}

/**
 * Wrap a horizontal scroller element (e.g. `<nav class="p-tabs">`) in a `.sstrip` container
 * that manages directional edge fades and a proportional custom scroll progress thumb.
 *
 * Returns `{ el, scroller, update, revealActive, destroy }`.
 */
export function scrollStrip(scroller, { activeSelector = '.on', reveal = true, className = '' } = {}) {
  scroller.classList.add('sstrip-in');
  const thumb = h('div', { class: 'sstrip-thumb' });
  const track = h('div', { class: 'sstrip-track', 'aria-hidden': 'true' }, thumb);
  const el = h('div', { class: 'sstrip' + (className ? ' ' + className : ''), dataset: { over: '0', fadeL: '0', fadeR: '0' } }, scroller, track);

  let raf = 0;
  let activeTimer = 0;
  let dead = false;

  function sync() {
    raf = 0;
    if (dead) return;
    const sw = scroller.scrollWidth;
    const cw = scroller.clientWidth;
    const maxScroll = Math.max(0, sw - cw);
    const over = maxScroll > 2;
    el.dataset.over = over ? '1' : '0';
    if (!over) {
      el.dataset.fadeL = '0';
      el.dataset.fadeR = '0';
      thumb.style.width = '';
      thumb.style.transform = '';
      return;
    }
    const sl = clamp(scroller.scrollLeft, 0, maxScroll);
    el.dataset.fadeL = sl > 2 ? '1' : '0';
    el.dataset.fadeR = sl < maxScroll - 2 ? '1' : '0';

    const trackW = track.clientWidth || cw;
    const ratio = cw / sw;
    const thumbW = clamp(Math.round(ratio * trackW), 28, trackW);
    const travel = Math.max(0, trackW - thumbW);
    const x = maxScroll > 0 ? Math.round((sl / maxScroll) * travel) : 0;
    thumb.style.width = thumbW + 'px';
    thumb.style.transform = `translate3d(${x}px, 0, 0)`;
  }

  function schedule() {
    if (dead || raf) return;
    raf = requestAnimationFrame(sync);
  }

  function markScrolling() {
    el.classList.add('scrolling');
    clearTimeout(activeTimer);
    activeTimer = setTimeout(() => el.classList.remove('scrolling'), 420);
  }

  function onScroll() {
    markScrolling();
    schedule();
  }

  function onWheel(e) {
    if (el.dataset.over !== '1') return;
    if (Math.abs(e.deltaY) > Math.abs(e.deltaX) && e.deltaY !== 0) {
      scroller.scrollLeft += e.deltaY;
      e.preventDefault();
    }
  }

  // Interactive track: tap or drag to scrub scroll position smoothly on mouse or touch.
  function scrubFromClientX(clientX) {
    const rect = track.getBoundingClientRect();
    if (!rect.width) return;
    const maxScroll = Math.max(0, scroller.scrollWidth - scroller.clientWidth);
    if (maxScroll <= 0) return;
    const pct = clamp((clientX - rect.left) / rect.width, 0, 1);
    scroller.scrollLeft = pct * maxScroll;
  }

  let dragging = false;
  function onTrackDown(e) {
    if (el.dataset.over !== '1') return;
    dragging = true;
    el.classList.add('scrolling');
    track.setPointerCapture?.(e.pointerId);
    scrubFromClientX(e.clientX);
    e.preventDefault();
  }
  function onTrackMove(e) {
    if (!dragging) return;
    scrubFromClientX(e.clientX);
  }
  function onTrackUp(e) {
    if (!dragging) return;
    dragging = false;
    track.releasePointerCapture?.(e.pointerId);
    markScrolling();
  }

  function revealActive({ smooth = false } = {}) {
    if (dead || !activeSelector) return;
    const candidates = [...scroller.querySelectorAll(activeSelector)];
    const active = candidates.find((n) => n.offsetParent !== null && n.offsetWidth > 0);
    if (!active) return;
    const targetLeft = active.offsetLeft;
    const targetRight = targetLeft + active.offsetWidth;
    const viewLeft = scroller.scrollLeft;
    const viewRight = viewLeft + scroller.clientWidth;
    const pad = 20;
    let next = null;
    if (targetLeft < viewLeft + pad) next = Math.max(0, targetLeft - pad);
    else if (targetRight > viewRight - pad) next = targetRight - scroller.clientWidth + pad;
    if (next !== null) {
      scroller.scrollTo({ left: next, behavior: smooth && !prefersReducedMotion() ? 'smooth' : 'auto' });
    }
    schedule();
  }

  scroller.addEventListener('scroll', onScroll, { passive: true });
  scroller.addEventListener('wheel', onWheel, { passive: false });
  track.addEventListener('pointerdown', onTrackDown);
  track.addEventListener('pointermove', onTrackMove);
  track.addEventListener('pointerup', onTrackUp);
  track.addEventListener('pointercancel', onTrackUp);

  const ro = typeof ResizeObserver === 'function' ? new ResizeObserver(schedule) : null;
  ro?.observe(el);
  ro?.observe(scroller);

  const mo = typeof MutationObserver === 'function' ? new MutationObserver(schedule) : null;
  mo?.observe(scroller, { childList: true, subtree: true, characterData: true, attributes: true, attributeFilter: ['class', 'hidden'] });

  const onWinResize = () => schedule();
  window.addEventListener('resize', onWinResize, { passive: true });
  document.fonts?.ready?.then(schedule).catch(() => {});

  // Initial measurement after layout, plus reveal active tab if clipped.
  requestAnimationFrame(() => {
    if (dead) return;
    sync();
    if (reveal) revealActive({ smooth: false });
  });

  function destroy() {
    if (dead) return;
    dead = true;
    if (raf) cancelAnimationFrame(raf);
    clearTimeout(activeTimer);
    scroller.removeEventListener('scroll', onScroll);
    scroller.removeEventListener('wheel', onWheel);
    track.removeEventListener('pointerdown', onTrackDown);
    track.removeEventListener('pointermove', onTrackMove);
    track.removeEventListener('pointerup', onTrackUp);
    track.removeEventListener('pointercancel', onTrackUp);
    window.removeEventListener('resize', onWinResize);
    ro?.disconnect();
    mo?.disconnect();
  }

  return { el, scroller, update: schedule, revealActive, destroy };
}
