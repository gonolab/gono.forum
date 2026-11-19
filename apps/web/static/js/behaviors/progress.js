// The progress behavior module (docs/skins.md, "Behavior modules"): on thread pages, a thin bar under the header that
// fills as the reader moves through the thread, and in the thread.outline progress blocks the floor being read out of
// all of them, with its page marked. Without it the block is the list of the thread's pages. The bar is decoration
// (hidden from screen readers); the block's <progress> element says the same in words. Data comes from the page data
// block: the page's floors, and the outline record of a progress or timeline block of this thread for its last floor;
// after a reply posted in place, the replies section's data-last-position. Blocks of another thread are left as lists.
(() => {
  if (window.gonofBehaviorProgress) return;
  window.gonofBehaviorProgress = true;
  const t = (key, args) => (window.gonofText ? window.gonofText(key, args) : key);
  const enabled = () => (document.documentElement.dataset.behaviors || '').split(' ').includes('progress');
  const stickyTop = () => (window.gonofStickyTop ? window.gonofStickyTop() : 0);
  let state = null;
  let frame = 0;

  function pageData() {
    try { return JSON.parse(document.getElementById('gonof-page')?.textContent || 'null'); } catch { return null; }
  }

  // The floor being read: the last floor, in page order, whose top has passed the reading line under the header (in
  // the nested view a reply's nested replies sit inside it, so the innermost one wins), or the last floor at the end
  // of the page.
  function readingFloor() {
    const floors = [...document.querySelectorAll('#main [id^="floor-"]')];
    if (!floors.length) return null;
    const line = stickyTop() + 24;
    const atEnd = scrollY + innerHeight >= document.documentElement.scrollHeight - 2;
    const passed = floors.filter(element => element.getBoundingClientRect().top <= line);
    const floor = atEnd ? floors[floors.length - 1] : passed[passed.length - 1] || floors[0];
    const position = Number(floor.id.slice('floor-'.length));
    return Number.isSafeInteger(position) ? position : null;
  }

  // The thread's highest floor as the replies section says it, which a reply posted in place swaps in; 0 when unknown.
  function lastPosition() {
    const value = Number(document.getElementById('topic-replies')?.dataset.lastPosition);
    return Number.isSafeInteger(value) && value > 0 ? value : 0;
  }

  let started = null;
  function start() {
    // Once per page: at once when this deferred script runs, and again at gonof:ready, which comes after.
    if (started === document.body) return;
    started = document.body;
    state?.bar.remove();
    state = null;
    if (!enabled()) return;
    const data = pageData();
    if (data?.type !== 'thread') return;
    // The thread's last floor: from an outline block of this thread when the page has one, else at least the replies
    // and this page.
    const outline = Object.values(data.blocks || {})
      .find(envelope => envelope?.source === 'thread.outline' && envelope.items?.[0]?.tid === data.tid)?.items?.[0];
    const floors = data.thread?.floors || [];
    const last = Math.max(outline?.last_position || 0, lastPosition(), (data.thread?.replies || 0) + 1, ...floors.map(floor => floor.position));
    const perPage = Math.max(1, outline?.per_page || 1);
    const bar = document.createElement('div');
    bar.className = 'reading-progress';
    bar.setAttribute('aria-hidden', 'true');
    const fill = document.createElement('div');
    fill.className = 'reading-progress__bar';
    bar.append(fill);
    document.body.append(bar);
    const blocks = [];
    for (const section of document.querySelectorAll('.block--thread-outline.block--progress[data-outline-tid]:not([data-enhanced])')) {
      if (Number(section.dataset.outlineTid) !== data.tid) continue;
      const line = document.createElement('p');
      line.className = 'outline-progress';
      const text = document.createElement('span');
      text.className = 'outline-progress__text';
      const meter = document.createElement('progress');
      meter.className = 'outline-progress__meter';
      meter.max = last;
      meter.setAttribute('aria-label', t('js-progress-label'));
      line.append(text, meter);
      section.querySelector('.panel__title')?.after(line);
      section.dataset.enhanced = 'progress';
      // Each link's page from its address (/thread-<id>-<page>; page 1 has none), since long threads list sampled pages.
      const links = [...section.querySelectorAll('.block-outline__pages a')].map(link => ({
        link,
        page: Number(link.pathname.match(/^.*\/thread-\d+-(\d+)$/)?.[1] || 1),
      }));
      blocks.push({ section, text, meter, links });
    }
    state = { bar, last, perPage, blocks };
    follow();
  }

  function follow() {
    frame = 0;
    if (!state) return;
    const position = readingFloor();
    if (position === null) return;
    const share = (position - 1) / Math.max(1, state.last - 1);
    state.bar.style.setProperty('--progress', share.toFixed(4));
    // The flat view's page of this floor, as the server pages it.
    const page = Math.floor(Math.max(0, position - 2) / state.perPage) + 1;
    for (const block of state.blocks) {
      block.meter.value = position;
      block.meter.setAttribute('aria-valuetext', t('js-progress-value', { position, last: state.last }));
      block.text.textContent = t('js-progress-value', { position, last: state.last });
      for (const { link, page: own } of block.links) {
        if (own === page) link.setAttribute('aria-current', 'page');
        else link.removeAttribute('aria-current');
      }
    }
  }

  addEventListener('scroll', () => { if (state && !frame) frame = requestAnimationFrame(follow); }, { passive: true });
  addEventListener('resize', () => { if (state && !frame) frame = requestAnimationFrame(follow); });
  document.addEventListener('gonof:ready', start);
  // A boosted navigation brings a new page into the same <body>.
  // A reply posted in place swaps the replies section, the page data block staying as it was: take the thread's highest
  // floor from the section.
  document.addEventListener('gonof:swap', event => {
    if (event.detail?.target === document.body) { started = null; start(); return; }
    const last = lastPosition();
    if (!state || last <= state.last) return;
    state.last = last;
    for (const block of state.blocks) block.meter.max = last;
    follow();
  });
  if (document.readyState !== 'loading') start();
})();
