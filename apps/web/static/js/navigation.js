// Must stay a synchronous <head> script loaded before htmx, as templates/layout/
// base.html does. The popstate handler below only works by registering first and
// stopping htmx's own listener; defer or async reverses that order and silently
// restores the stale-content path. No test covers this: navigation.spec.js builds
// its own shell, so a loading attribute added in base.html fails nothing.
(() => {
  if (window.gonofNavigationInitialized) return;
  window.gonofNavigationInitialized = true;
  const key = 'gonofNavigation';
  const url = () => location.pathname + location.search;
  const read = () => {
    const value = history.state?.[key];
    return value?.url === url() && typeof value.id === 'string' ? value : null;
  };
  const fresh = () => ({ id: `${Date.now()}-${Math.random().toString(36).slice(2)}`, url: url() });
  let entry = read() || fresh();
  let ready = false;
  let reloading = false;
  let departure = null;
  let scrollTimer = 0;
  let cancelled = false;
  const initialRestore = entry.restore || performance.getEntriesByType('navigation')[0]?.type === 'back_forward';
  const position = entry.position;
  const restoring = initialRestore && !location.hash && Number.isFinite(position?.y);
  if (restoring) history.scrollRestoration = 'manual';
  // The height the sticky header covers, declared in CSS as --sticky-top (docs/skins.md, "Sticky header") rather than
  // measured from the header, so a layout without the top header can declare its own. The stylesheet registers it as a
  // length, so it reads back resolved ("61px"); a browser without @property returns the calc() as written, and a probe
  // element lets the browser resolve that.
  function stickyTop() {
    const value = parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--sticky-top'));
    if (Number.isFinite(value)) return value;
    const probe = document.createElement('div');
    probe.style.setProperty('position', 'absolute');
    probe.style.setProperty('visibility', 'hidden');
    probe.style.setProperty('block-size', 'var(--sticky-top)');
    document.documentElement.append(probe);
    const height = probe.getBoundingClientRect().height;
    probe.remove();
    return height || 0;
  }

  function write() {
    try { history.replaceState({ ...history.state, [key]: entry }, ''); }
    catch { /* History may be unavailable; retain server validation without recovery. */ }
  }
  // No HTML, account data or credentials are kept in browser history.
  function save(force = false) {
    if ((!ready && !force) || reloading || entry.url !== url() || (!force && read()?.id !== entry.id)) return;
    const list = document.querySelector('[data-scroll-list]');
    if (list) {
      const items = [...list.querySelectorAll('[data-thread-id]')];
      const header = stickyTop();
      // Only a row the reader can see anchors the position: when the list starts below the window (a board
      // directory above it, on a phone), restoring by a row out of view would scroll the page to that row; then the
      // scroll offset alone is kept.
      const visible = item => {
        const box = item.getBoundingClientRect();
        return box.bottom > header && box.top < innerHeight;
      };
      const candidate = items.find(item => item.dataset.threadId === departure)
        || items.find(item => item.getBoundingClientRect().bottom > header);
      const anchor = candidate && visible(candidate) ? candidate : null;
      entry.position = { y: scrollY, tid: anchor?.dataset.threadId, offset: anchor?.getBoundingClientRect().top };
    }
    write();
  }
  write();

  function revalidate() {
    if (reloading) return;
    reloading = true;
    entry = read() || fresh();
    entry.restore = true;
    write();
    // Keep the original pageshow reload guarantee: all authentication and page
    // visibility checks run on the server before restoring only the scroll position.
    document.documentElement.setAttribute('data-history-revalidating', '');
    location.reload();
  }
  window.addEventListener('pageshow', event => { if (event.persisted) revalidate(); });
  // Installed before HTMX. Its history-miss path can leave stale content on a
  // 403/404; a full navigation renders the current server result in every case.
  window.addEventListener('popstate', event => {
    if (reloading) { event.stopImmediatePropagation(); return; }
    if (event.state?.htmx) {
      event.stopImmediatePropagation();
      if (event.state?.[key]?.id !== entry.id) revalidate();
    }
  });
  window.addEventListener('pagehide', () => save());
  window.addEventListener('scroll', () => {
    if (!ready || reloading) return;
    departure = null;
    clearTimeout(scrollTimer);
    // Avoid Safari's history mutation rate limit; navigation hooks flush immediately.
    scrollTimer = setTimeout(() => { scrollTimer = 0; save(); }, 500);
  }, { passive: true });
  document.addEventListener('click', event => {
    if (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
    const link = event.target instanceof Element ? event.target.closest('a[href]') : null;
    if (!link || link.target === '_blank' || link.hasAttribute('download')) return;
    departure = link.closest('[data-scroll-list] [data-thread-id]')?.dataset.threadId || null;
    save();
  }, true);
  // HTMX replaces history.state while saving the outgoing page. Restore our
  // entry metadata at its public hook, then assign a distinct id to the new entry.
  document.addEventListener('htmx:beforeHistoryUpdate', () => save(true));
  for (const event of ['htmx:pushedIntoHistory', 'htmx:replacedInHistory']) {
    document.addEventListener(event, () => {
      entry = fresh();
      ready = false;
      departure = null;
      write();
    });
  }
  document.addEventListener('htmx:afterSettle', () => {
    requestAnimationFrame(() => { if (!reloading) { ready = true; save(); } });
  });
  // A boosted navigation replaces the side navigation (docs/skins.md, "Side navigation") with the new page's copy,
  // scrolled to the top; keep where the reader had scrolled it, so the column does not jump on every click.
  let sideScroll = null;
  document.addEventListener('htmx:beforeSwap', event => {
    if (event.detail?.boosted && event.detail.target === document.body) {
      sideScroll = document.querySelector('[data-side-nav]')?.scrollTop ?? null;
    }
  });
  document.addEventListener('htmx:afterSwap', event => {
    if (sideScroll === null || event.detail?.target !== document.body) return;
    const side = document.querySelector('[data-side-nav]');
    if (side) side.scrollTop = sideScroll;
    sideScroll = null;
  });

  const cancel = () => { cancelled = true; };
  for (const event of ['wheel', 'touchstart', 'keydown', 'pointerdown']) {
    window.addEventListener(event, cancel, { passive: true, once: true });
  }
  window.addEventListener('load', async () => {
    delete entry.restore;
    write();
    if (restoring && document.querySelector('[data-scroll-list]')) {
      await document.fonts.ready;
      await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
      if (!cancelled && !reloading) {
        const anchor = [...document.querySelectorAll('[data-scroll-list] [data-thread-id]')]
          .find(item => item.dataset.threadId === position.tid);
        const header = stickyTop();
        const lowest = anchor ? header - anchor.getBoundingClientRect().height + 1 : header;
        const offset = Math.max(lowest, Math.min(position.offset ?? header, innerHeight - 24));
        const top = anchor ? scrollY + anchor.getBoundingClientRect().top - offset : position.y;
        window.scrollTo({ top: Math.max(0, top), behavior: 'instant' });
      }
    }
    if (restoring) history.scrollRestoration = 'auto';
    ready = true;
    save();
  }, { once: true });
})();
