// The timeline behavior module (docs/skins.md, "Behavior modules"): turns the thread.outline timeline blocks of a
// thread page into a scrubber, a vertical slider over every floor of the thread with the reader's floor and the date
// around it. Without it (no JavaScript, or a skin that does not enable it) the block is a list of the thread's pages
// with the date each begins. Data comes from the page data block: the block's outline record and the page's floors.
// Keyboard: arrows move one floor, Page Up and Page Down one page, Home and End to the ends; Enter goes there and
// Escape goes back to the floor being read. Scrolling follows prefers-reduced-motion. A block of another thread than
// the page's (`tid` set to an id) only opens that thread's floors and does not follow the reading.
(() => {
  if (window.gonofBehaviorTimeline) return;
  window.gonofBehaviorTimeline = true;
  const t = (key, args) => (window.gonofText ? window.gonofText(key, args) : key);
  const enabled = () => (document.documentElement.dataset.behaviors || '').split(' ').includes('timeline');
  const still = () => matchMedia('(prefers-reduced-motion: reduce)').matches;
  const stickyTop = () => (window.gonofStickyTop ? window.gonofStickyTop() : 0);
  let timelines = [];
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

  function build(section, data) {
    const outline = data.blocks?.[section.dataset.block]?.items?.[0];
    if (!outline || !(outline.last_position > 1)) return null;
    const base = data.site?.base || '';
    const own = outline.tid === data.tid;
    const last = own ? Math.max(outline.last_position, lastPosition()) : outline.last_position;
    const perPage = Math.max(1, outline.per_page || 1);
    // The flat view's page of a floor, as the server pages it: the opening post and the first replies on page 1.
    const pageOf = position => Math.floor(Math.max(0, position - 2) / perPage) + 1;
    const href = position => {
      const page = pageOf(position);
      return `${base}/thread-${outline.tid}${page > 1 ? `-${page}` : ''}?view=flat#floor-${position}`;
    };
    // The marks with a known time, by floor: page starts and the first floor of each day or month.
    const marks = [
      ...(outline.page_marks || []).map(mark => ({ position: mark.from, at: mark.first_at })),
      ...(outline.date_marks || []).map(mark => ({ position: mark.position, at: mark.at })),
      { position: outline.first.position, at: outline.first.at },
      { position: outline.last.position, at: outline.last.at },
    ].filter(mark => mark.at).sort((a, b) => a.position - b.position);
    const dateOf = position => {
      let found = marks[0];
      for (const mark of marks) { if (mark.position <= position) found = mark; else break; }
      return found?.at?.text || '';
    };

    const box = document.createElement('div');
    box.className = 'timeline';
    box.dataset.timeline = '';
    const end = (position, at, modifier) => {
      const link = document.createElement('a');
      link.className = `timeline__end timeline__end--${modifier}`;
      link.href = href(position);
      const time = document.createElement('time');
      time.dateTime = at.at;
      time.title = at.title;
      time.textContent = at.text;
      link.append(time);
      return link;
    };
    const track = document.createElement('div');
    track.className = 'timeline__track';
    const rail = document.createElement('div');
    rail.className = 'timeline__rail';
    rail.setAttribute('aria-hidden', 'true');
    const fill = document.createElement('div');
    fill.className = 'timeline__fill';
    rail.append(fill);
    const handle = document.createElement('div');
    handle.className = 'timeline__handle';
    handle.tabIndex = 0;
    handle.setAttribute('role', 'slider');
    handle.setAttribute('aria-orientation', 'vertical');
    handle.setAttribute('aria-label', t('js-timeline-label'));
    handle.setAttribute('aria-valuemin', '1');
    handle.setAttribute('aria-valuemax', String(last));
    const hint = document.createElement('p');
    hint.className = 'sr-only';
    hint.id = `timeline-hint-${section.dataset.block}`;
    hint.textContent = t('js-timeline-hint');
    handle.setAttribute('aria-describedby', hint.id);
    const position = document.createElement('span');
    position.className = 'timeline__position';
    const date = document.createElement('span');
    date.className = 'timeline__date';
    handle.append(position, date);
    track.append(rail, handle);
    box.append(end(outline.first.position, outline.first.at, 'first'), track, end(outline.last.position, outline.last.at, 'last'), hint);

    const state = { section, box, track, handle, fill, position, date, own, last, perPage, href, dateOf, reading: 1, preview: null, grab: 0, pressed: null };
    // The static page list stays for readers of the markup; the scrubber replaces it on screen.
    const list = section.querySelector('.block-outline__pages');
    if (list) list.hidden = true;
    section.append(box);
    section.dataset.enhanced = 'timeline';

    // Where on the handle it was taken, so it moves with the pointer instead of jumping to put its top there; a press
    // on the rail takes the handle by its middle. Pressing the handle and letting go without moving it goes nowhere.
    track.addEventListener('pointerdown', event => {
      if (event.button !== 0) return;
      event.preventDefault();
      handle.focus({ preventScroll: true });
      track.setPointerCapture(event.pointerId);
      const box = handle.getBoundingClientRect();
      const onHandle = event.clientY >= box.top && event.clientY <= box.bottom;
      state.grab = onHandle ? event.clientY - box.top : box.height / 2;
      state.pressed = onHandle ? state.preview ?? state.reading : null;
      preview(state, fromPointer(state, event));
    });
    track.addEventListener('pointermove', event => {
      if (track.hasPointerCapture(event.pointerId)) preview(state, fromPointer(state, event));
    });
    track.addEventListener('pointerup', event => {
      if (!track.hasPointerCapture(event.pointerId)) return;
      track.releasePointerCapture(event.pointerId);
      const target = state.preview ?? state.reading;
      if (state.pressed === target) { state.preview = null; draw(state); }
      else go(state, target);
      state.pressed = null;
    });
    handle.addEventListener('keydown', event => {
      const current = state.preview ?? state.reading;
      const step = {
        ArrowUp: -1, ArrowLeft: -1, ArrowDown: 1, ArrowRight: 1,
        PageUp: -state.perPage, PageDown: state.perPage,
      }[event.key];
      if (step) preview(state, current + step);
      else if (event.key === 'Home') preview(state, 1);
      else if (event.key === 'End') preview(state, state.last);
      else if (event.key === 'Enter' || event.key === ' ') go(state, current);
      else if (event.key === 'Escape' && state.preview !== null) { state.preview = null; draw(state); }
      else return;
      event.preventDefault();
    });
    handle.addEventListener('blur', () => { if (state.preview !== null) { state.preview = null; draw(state); } });
    return state;
  }

  // The handle's top moves over the track's height less its own (behaviors.css), so that is the length a floor share is
  // measured against.
  function fromPointer(state, event) {
    const rect = state.track.getBoundingClientRect();
    const travel = Math.max(1, rect.height - state.handle.getBoundingClientRect().height);
    const share = Math.min(1, Math.max(0, (event.clientY - state.grab - rect.top) / travel));
    return Math.round(1 + share * (state.last - 1));
  }

  function preview(state, position) {
    state.preview = Math.min(state.last, Math.max(1, position));
    draw(state);
  }

  function draw(state) {
    const position = state.preview ?? state.reading;
    const share = (position - 1) / Math.max(1, state.last - 1);
    state.box.style.setProperty('--timeline-at', share.toFixed(4));
    state.box.classList.toggle('is-previewing', state.preview !== null);
    const date = state.dateOf(position);
    state.position.textContent = t('js-timeline-short', { position, last: state.last });
    state.date.textContent = date;
    state.handle.setAttribute('aria-valuemax', String(state.last));
    state.handle.setAttribute('aria-valuenow', String(position));
    state.handle.setAttribute('aria-valuetext', date ? t('js-timeline-value', { position, last: state.last, date }) : t('js-timeline-value-plain', { position, last: state.last }));
  }

  // Goes to a floor: scrolls to it when it is on this page of the same thread, else opens its page of the flat view.
  function go(state, position) {
    state.preview = null;
    const floor = state.own ? document.getElementById(`floor-${position}`) : null;
    if (floor) {
      floor.scrollIntoView({ block: 'start', behavior: still() ? 'auto' : 'smooth' });
      history.replaceState(history.state, '', `${location.pathname}${location.search}#floor-${position}`);
      state.reading = position;
      draw(state);
    } else {
      location.assign(state.href(position));
    }
  }

  function follow() {
    frame = 0;
    const position = readingFloor();
    if (position === null) return;
    for (const state of timelines) {
      if (!state.section.isConnected || !state.own) continue;
      state.reading = Math.min(state.last, position);
      if (state.preview === null) draw(state);
    }
  }

  // A reply posted in place swaps the replies section, the page data block staying as it was: the timelines of this
  // thread take its highest floor from the section.
  function extend() {
    const last = lastPosition();
    for (const state of timelines) {
      if (!state.section.isConnected || !state.own || last <= state.last) continue;
      state.last = last;
      draw(state);
    }
    follow();
  }

  let started = null;
  function start() {
    // Once per page: at once when this deferred script runs, and again at gonof:ready, which comes after.
    if (started === document.body) return;
    started = document.body;
    timelines = timelines.filter(state => state.section.isConnected);
    if (!enabled()) return;
    const data = pageData();
    if (data?.type !== 'thread') return;
    for (const section of document.querySelectorAll('.block--thread-outline.block--timeline[data-outline-tid]:not([data-enhanced])')) {
      const state = build(section, data);
      if (state) timelines.push(state);
    }
    follow();
  }

  addEventListener('scroll', () => { if (timelines.length && !frame) frame = requestAnimationFrame(follow); }, { passive: true });
  addEventListener('resize', () => { if (timelines.length && !frame) frame = requestAnimationFrame(follow); });
  document.addEventListener('gonof:ready', start);
  // A boosted navigation brings a new page into the same <body>.
  document.addEventListener('gonof:swap', event => {
    if (event.detail?.target === document.body) { started = null; start(); }
    else if (timelines.length) extend();
  });
  // Deferred scripts run once the page is parsed; one added by a boosted navigation later. Either way, start now.
  if (document.readyState !== 'loading') start();
})();
