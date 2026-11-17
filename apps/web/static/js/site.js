(() => {
  if (window.gonofInitialized) return;
  window.gonofInitialized = true;
  document.documentElement.dataset.js = 'true';
  document.addEventListener('htmx:configRequest', event => { const nonce = document.querySelector('meta[name=csp-nonce]')?.content; if (nonce) event.detail.headers['X-CSP-Nonce'] = nonce; });
  // Interface text comes from /static/i18n/<language>.js, loaded just before this file (docs/architecture.md,
  // "Languages"); pages without it show the keys.
  const t = (name, args) => (window.gonofT ? window.gonofT(name, args) : name);
  window.gonofText = t;
  const key = 'gonof-theme';
  let theme = 'system';
  try { theme = localStorage.getItem(key) || 'system'; } catch {}
  function applyTheme() {
    if (theme === 'light' || theme === 'dark') document.documentElement.dataset.theme = theme;
    else document.documentElement.removeAttribute('data-theme');
    for (const button of document.querySelectorAll('[data-close-menu]')) button.hidden = false;
    const still = matchMedia('(prefers-reduced-motion: reduce)').matches;
    for (const button of document.querySelectorAll('[data-theme-toggle]')) {
      button.hidden = false;
      if (still) for (const svg of button.querySelectorAll('svg')) svg.pauseAnimations?.();
      const label = t(theme === 'light' || theme === 'dark' ? `js-theme-${theme}` : 'js-theme-system');
      button.setAttribute('aria-label', label);
      button.title = t('js-theme-title', { label });
    }
  }
  applyTheme();
  document.addEventListener('DOMContentLoaded', applyTheme);
  document.addEventListener('htmx:afterSwap', applyTheme);
  // The reading position is written into the address while scrolling (see the floor tracking at the end of the
  // file) and used directly when switching views.
  // The switch link sits at the top of the page, so readers must scroll back up to click it; judging the visible
  // floor only at the moment of the click would always give floor 1. The cross-page mapping is written by the
  // server in data-alternate-href.
  function readingFloor() {
    // Without an anchor this must yield null: an empty string is not nullish, so ?. would not short-circuit and
    // would throw on the following property.
    const fromHash = location.hash ? document.getElementById(location.hash.slice(1)) : null;
    return fromHash?.dataset.alternateHref ? fromHash : null;
  }
  document.addEventListener('click', (event) => {
    const switchLink = event.target.closest('[data-view-switch]');
    // When the floor cannot be found, keep the server-rendered href, falling back to the first page of the other
    // view.
    if (switchLink) {
      const floor = readingFloor();
      if (floor?.dataset.alternateHref) switchLink.href = floor.dataset.alternateHref;
    }
    if (event.target.closest('[data-theme-toggle]')) {
      theme = { system: 'light', light: 'dark', dark: 'system' }[theme] || 'system';
      try { localStorage.setItem(key, theme); } catch {}
      applyTheme();
    }
    const menu = document.querySelector('.mobile-nav');
    if (menu && (event.target === menu || event.target.closest('[data-close-menu]') || event.target.closest('.drawer__link') || !menu.contains(event.target))) {
      const restoreFocus = menu.open && (event.target === menu || event.target.closest('[data-close-menu]'));
      menu.open = false;
      if (restoreFocus) menu.querySelector('summary').focus();
    }
  });
  // The drawer is the navigation from 900px down, and from 1099px down beside side navigation, where the side
  // navigation folds into it (docs/skins.md, "Side navigation").
  const drawerShown = () => matchMedia(document.documentElement.dataset.nav === 'side' ? '(max-width: 1099px)' : '(max-width: 900px)').matches;
  document.addEventListener('toggle', event => {
    const menu = event.target;
    if (menu.matches?.('.mobile-nav') && menu.isConnected && menu.open && drawerShown()) {
      menu.querySelector('[data-close-menu]').focus();
    }
  }, true);
  document.addEventListener('keydown', (event) => {
    const menu = document.querySelector('.mobile-nav[open]');
    if (event.key === 'Tab' && menu && drawerShown()) {
      const controls = [...menu.querySelectorAll('.drawer a[href], .drawer button')].filter(element => !element.disabled && element.getClientRects().length);
      const first = controls[0], last = controls[controls.length - 1];
      if (first && (!controls.includes(document.activeElement) || (event.shiftKey && document.activeElement === first) || (!event.shiftKey && document.activeElement === last))) {
        event.preventDefault();
        (event.shiftKey ? last : first).focus();
      }
    }
    if (event.key === 'Escape') {
      if (menu) { menu.open = false; menu.querySelector('summary').focus(); }
    }
  });
  document.addEventListener('htmx:beforeSwap', event => {
    const detail = event.detail;
    if (!detail?.boosted || detail.target !== document.body) return;
    const status = detail.xhr?.status;
    if (status < 400 || status > 599) return;
    const contentType = detail.xhr.getResponseHeader('Content-Type') || '';
    const html = detail.serverResponse;
    if (!/^text\/html(?:\s*;|\s*$)/i.test(contentType) || typeof html !== 'string') return;
    if (!/^\s*(?:<!doctype\s+html[^>]*>\s*)?<html(?:\s|>)/i.test(html) || !/<\/html\s*>\s*$/i.test(html)) return;
    // Full error pages are updated together with the identity state; fragments and non-HTML errors keep the
    // original behavior.
    detail.shouldSwap = true;
  });
  // After hx-boost swaps the whole body, the document title changes but screen readers do not announce it, and the
  // Tab starting point stays at the old position.
  // Move focus to the #main that the skip link already targets, matching full-page navigation; preventScroll keeps
  // the scrolling and floor positioning of navigation.js, otherwise returning to a list page would pull it back to
  // the top.
  document.addEventListener('htmx:afterSettle', event => {
    const detail = event.detail;
    if (!detail?.boosted || detail.target !== document.body) return;
    document.getElementById('main')?.focus({ preventScroll: true });
  });
  function revealInviteCopy() {
    for (const button of document.querySelectorAll('[data-invite-copy]')) button.hidden = false;
  }
  document.addEventListener('DOMContentLoaded', revealInviteCopy);
  document.addEventListener('htmx:afterSwap', revealInviteCopy);
  document.addEventListener('click', async event => {
    if (!(event.target instanceof Element)) return;
    const account = document.querySelector('.account-nav');
    if (account && !account.contains(event.target)) account.open = false;
    const button = event.target.closest('[data-invite-copy]');
    if (!button) return;
    const row = button.closest('[data-invite-id]');
    const code = row.querySelector('[data-invite-code]');
    const status = row.querySelector('[data-invite-copy-status]');
    try { await navigator.clipboard.writeText(code.textContent); status.textContent = t('js-copy-done'); }
    catch {
      const range = document.createRange(); range.selectNodeContents(code);
      const selection = window.getSelection(); selection.removeAllRanges(); selection.addRange(range);
      status.textContent = t('js-copy-selected');
    }
  });
  document.addEventListener('keydown', event => {
    if (event.key !== 'Escape') return;
    const account = document.querySelector('.account-nav[open]');
    if (account) { account.open = false; account.querySelector('summary').focus(); }
  });

// A boosted navigation swaps the body and merges the head but keeps <html>, whose attributes carry the skin, palette
// and avatar shape the stylesheets select on (docs/skins.md, "Frame"). Take them from the new page in the same tick
// as head-support adds its skin stylesheet, so a page with another skin (such as /manage/skins, which always uses
// the default) never pairs one skin's stylesheet with another's palette name.
const frameAttributes = ['data-skin', 'data-skin-source', 'data-palette', 'data-nav', 'data-header-boards', 'data-avatar-shape'];
document.addEventListener('htmx:afterSwap', event => {
  if (!event.detail?.boosted) return;
  const tag = (event.detail.xhr?.responseText || '').match(/<html\b[^>]*>/i);
  if (!tag) return;
  const next = new DOMParser().parseFromString(`${tag[0]}</html>`, 'text/html').documentElement;
  for (const name of frameAttributes) {
    const value = next.getAttribute(name);
    if (value === null) document.documentElement.removeAttribute(name);
    else if (document.documentElement.getAttribute(name) !== value) document.documentElement.setAttribute(name, value);
  }
});

// Posting responses replace only the reply section. Error fragments are deliberate
// form responses, and retain the same editor fields and already uploaded files.
document.addEventListener('htmx:beforeSwap', event => {
  const d = event.detail;
  if (d?.xhr?.getResponseHeader('X-Composer-Fragment') === 'true' || d?.xhr?.getResponseHeader('X-Interaction-Page') === 'true') {
    d.shouldSwap = true; d.isError = false;
  }
});
document.addEventListener('composer:locate', event => {
  const position = event.detail?.position;
  if (Number.isSafeInteger(position) && position > 0) {
    const hash = `#floor-${position}`;
    history.replaceState(history.state, '', location.pathname + location.search + hash);
    requestAnimationFrame(() => document.getElementById(hash.slice(1))?.scrollIntoView({block:'center'}));
  }
});

// After switching views or jumping to a floor, briefly mark the target floor to help regain context. The class is
// removed by the script, so it stays visible when prefers-reduced-motion turns off the transition, just without the
// fade-out.
let floorFlashTimer = 0;
function flashTargetFloor() {
  const target = location.hash ? document.getElementById(location.hash.slice(1)) : null;
  if (!target?.id.startsWith('floor-')) return;
  clearTimeout(floorFlashTimer);
  for (const stale of document.querySelectorAll('.floor--flash')) stale.classList.remove('floor--flash');
  target.classList.add('floor--flash');
  floorFlashTimer = setTimeout(() => target.classList.remove('floor--flash'), 1600);
}
window.addEventListener('load', flashTargetFloor);
window.addEventListener('hashchange', flashTargetFloor);

// Reading-position tracking on thread pages: while scrolling, write the current floor into the address, and after
// the viewport changes reposition by floor rather than by the old scroll pixels. The address is updated with
// replaceState, without adding history entries. The floor container is looked up on every use: boosted navigation
// swaps the whole body (a thread opened from the home page has no container at DOMContentLoaded), and a reply
// replaces the floor list. Each page load and swap starts over, so no anchor or timer of the page before is used.
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
{
  const repliesList = () => document.getElementById('topic-replies');
  let anchor = null;
  let offset = 0;
  let scrollTimer = 0;
  let resizeTimer = 0;
  let width = innerWidth;
  let height = innerHeight;
  function locate() {
    const replies = repliesList();
    const header = stickyTop();
    const floors = replies ? [...replies.querySelectorAll('[id^="floor-"]')] : [];
    // A floor showing only its bottom edge does not count as "being read"; a threshold of about one line of text
    // avoids jitter at the boundary.
    anchor = floors.find(floor => floor.getBoundingClientRect().bottom > header + 24)
      || floors[floors.length - 1] || null;
    offset = anchor ? anchor.getBoundingClientRect().top - header : 0;
  }
  function publish() {
    // The view-switch link is at the top of the page, and by the time readers scroll back up to click it the
    // visible floor has become floor 1; so the position must be recorded while scrolling, not judged at the moment
    // of the click.
    if (!anchor?.isConnected || location.hash === `#${anchor.id}`) return;
    history.replaceState(history.state, '', `${location.pathname}${location.search}#${anchor.id}`);
  }
  function restart() {
    clearTimeout(scrollTimer);
    clearTimeout(resizeTimer);
    scrollTimer = 0;
    resizeTimer = 0;
    locate();
  }
  // This file is a synchronous script in the page head and the DOM is not parsed yet at first.
  document.addEventListener('DOMContentLoaded', restart);
  document.addEventListener('htmx:afterSettle', restart);
  addEventListener('scroll', () => {
    if (resizeTimer || !repliesList()) return;
    locate();
    clearTimeout(scrollTimer);
    // Debounced to stay under Safari's rate limit on history rewrites.
    scrollTimer = setTimeout(() => { scrollTimer = 0; publish(); }, 250);
  }, { passive: true });
  addEventListener('resize', () => {
    if (innerWidth === width && innerHeight === height) return;
    width = innerWidth;
    height = innerHeight;
    if (!anchor?.isConnected) return;
    clearTimeout(resizeTimer);
    resizeTimer = setTimeout(() => {
      resizeTimer = 0;
      if (!anchor?.isConnected) return;
      const top = scrollY + anchor.getBoundingClientRect().top - stickyTop() - offset;
      scrollTo({ top: Math.max(0, top), behavior: 'instant' });
    }, 120);
  });
}

// The "back to top / go to bottom" buttons at the bottom right of long pages. Pure progressive enhancement: the buttons are
// created by the script and do not appear without JS; they are not shown when the page is less than two screens
// tall, "back to top" is hidden near the top and "go to bottom" near the bottom.
// After hx-boost swaps the whole body the buttons go with it, so they are re-attached in afterSettle.
const jumpIcons = {
  top: '<svg viewBox="0 0 24 24" fill="none" aria-hidden="true"><path d="M12 19V5m-6 6 6-6 6 6" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></svg>',
  bottom: '<svg viewBox="0 0 24 24" fill="none" aria-hidden="true"><path d="M12 5v14m6-6-6 6-6-6" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></svg>',
};
let jumpFrame = 0;
function pageJump() {
  let box = document.querySelector('[data-page-jump]');
  if (box || !document.body) return box;
  box = document.createElement('div');
  box.className = 'page-jump';
  box.dataset.pageJump = '';
  for (const [direction, label] of [['top', window.gonofText('js-jump-top')], ['bottom', window.gonofText('js-jump-bottom')]]) {
    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'page-jump-btn';
    button.dataset.jump = direction;
    button.setAttribute('aria-label', label);
    button.title = label;
    button.innerHTML = jumpIcons[direction];
    button.hidden = true;
    box.append(button);
  }
  document.body.append(box);
  return box;
}
function updatePageJump() {
  jumpFrame = 0;
  const box = pageJump();
  if (!box) return;
  const height = document.documentElement.scrollHeight;
  const view = innerHeight;
  const long = height > view * 2;
  box.querySelector('[data-jump="top"]').hidden = !long || scrollY < view / 2;
  box.querySelector('[data-jump="bottom"]').hidden = !long || scrollY + view > height - view / 2;
}
function schedulePageJump() {
  if (!jumpFrame) jumpFrame = requestAnimationFrame(updatePageJump);
}
// Compute once directly on load and page change: requestAnimationFrame does not run in background tabs, so it is
// only used to throttle scrolling.
document.addEventListener('DOMContentLoaded', updatePageJump);
document.addEventListener('htmx:afterSettle', updatePageJump);
addEventListener('load', updatePageJump);
addEventListener('scroll', schedulePageJump, { passive: true });
addEventListener('resize', schedulePageJump);
document.addEventListener('click', event => {
  const button = event.target instanceof Element ? event.target.closest('[data-jump]') : null;
  if (!button) return;
  const behavior = matchMedia('(prefers-reduced-motion: reduce)').matches ? 'auto' : 'smooth';
  if (button.dataset.jump === 'top') {
    scrollTo({ top: 0, behavior });
    // After keyboard users return to the top, Tab continues from the start of the content rather than staying on
    // the button at the very bottom of the page.
    document.getElementById('main')?.focus({ preventScroll: true });
  } else {
    scrollTo({ top: document.documentElement.scrollHeight, behavior });
  }
});

// The header "More" menu: closes on a click outside the menu or on Escape. Without JS the native details still opens
// and closes as usual; it just does not close on an outside click.
document.addEventListener('click', event => {
  for (const more of document.querySelectorAll('.forum-more[open]')) {
    if (!more.contains(event.target)) more.open = false;
  }
});
document.addEventListener('keydown', event => {
  const more = event.key === 'Escape' ? document.querySelector('.forum-more[open]') : null;
  if (more) { more.open = false; more.querySelector('summary').focus(); }
});

// On narrow screens the admin module bar (.admin-menu) and the tab bar (.section-tabs) scroll horizontally with the
// scrollbar hidden: a current item further along (such as "Tools & maintenance" or "Verification guidelines") is out of
// view as soon as the page loads, so you cannot tell where you are. After load and page change, scroll the current item
// into the center; only the scrollLeft of these two navs changes, the page itself does not scroll, and nothing moves
// when there is no overflow or the item is already visible.
// Without JS they stay at the start and can still be swiped horizontally.
function revealCurrentNav() {
  for (const bar of document.querySelectorAll('.admin-menu, .section-tabs')) {
    const current = bar.querySelector('[aria-current="page"]');
    if (!current || bar.scrollWidth <= bar.clientWidth) continue;
    const outer = bar.getBoundingClientRect(), inner = current.getBoundingClientRect();
    if (inner.left >= outer.left && inner.right <= outer.right) continue;
    bar.scrollLeft += inner.left - outer.left - (bar.clientWidth - inner.width) / 2;
  }
}
document.addEventListener('DOMContentLoaded', revealCurrentNav);
document.addEventListener('htmx:afterSettle', revealCurrentNav);

// Recently viewed: stored only in this browser's localStorage, never recorded by the server. Written when a thread
// page carries data-recent-thread, read by the data-recent-threads card in the home page sidebar. Stored under
// separate keys by the page's data-recent-owner (member uid, 0 for visitors), so after switching accounts or signing
// out nobody else's records are visible. The key also holds the forum's path prefix (<html data-base>): forums under
// different paths of one host share the storage, and a thread id of one is not a thread of another. Titles come from
// the page itself and are displayed only through textContent; addresses are rebuilt from the thread id, never
// trusting arbitrary strings in storage.
const recentLimit = 10;
const recentKey = owner => `gonof:${encodeURIComponent(document.documentElement.dataset.base || '/')}:recent-threads:${/^\d+$/.test(owner || '') ? owner : '0'}`;
function readRecent(owner) {
  let items = [];
  try { items = JSON.parse(localStorage.getItem(recentKey(owner)) || '[]'); } catch { return null; }
  if (!Array.isArray(items)) return [];
  return items.filter(item => Number.isSafeInteger(item?.id) && item.id > 0
    && typeof item.title === 'string' && Number.isFinite(item.time)).slice(0, recentLimit);
}
function writeRecent(owner, items) {
  try {
    if (items.length) localStorage.setItem(recentKey(owner), JSON.stringify(items));
    else localStorage.removeItem(recentKey(owner));
    return true;
  } catch { return false; }
}
function recordRecent() {
  const marker = document.querySelector('[data-recent-thread]');
  const id = Number(marker?.dataset.recentThread);
  if (!marker || !Number.isSafeInteger(id) || id <= 0) return;
  const owner = marker.dataset.recentOwner;
  const items = readRecent(owner);
  if (!items) return;
  const title = (marker.dataset.recentTitle || '').trim().slice(0, 200) || t('js-recent-untitled', { id: String(id) });
  writeRecent(owner, [{ id, title, url: `/thread-${id}`, time: Date.now() },
    ...items.filter(item => item.id !== id)].slice(0, recentLimit));
}
// The viewed time is written following the site's time settings, matching the server's clock::short_datetime: with
// relative times on (data-relative-hours greater than 0) and within range it reads "3 hours ago"; otherwise it is the
// date and time without the year, in the site time zone.
function recentFormat(card) {
  const integer = value => (Number.isInteger(Number(value)) && value !== '' ? Number(value) : null);
  return {
    hours: integer(card.dataset.relativeHours) ?? 0,
    // The page always writes the site time zone; should it be missing, display in the browser's time zone.
    offset: integer(card.dataset.offsetMinutes) ?? -new Date().getTimezoneOffset(),
    date: card.dataset.dateFormat || 'ymd-dash',
    clock24: card.dataset.clock24 !== 'false',
  };
}
function recentTime(time, format) {
  const elapsed = Math.floor((Date.now() - time) / 1000);
  if (format.hours > 0 && elapsed >= 0 && elapsed < format.hours * 3600) {
    if (elapsed < 60) return t('js-time-just-now');
    if (elapsed < 3600) return t('js-time-minutes-ago', { n: Math.floor(elapsed / 60) });
    return t('js-time-hours-ago', { n: Math.floor(elapsed / 3600) });
  }
  // Add the site time zone offset first, then read each field as UTC, giving the wall-clock time in the site time
  // zone.
  const at = new Date(time + format.offset * 60000);
  if (format.date === 'locale') {
    // "Follow the language" (docs/architecture.md, "Languages"): the browser's Intl in the page language, like the
    // server's ICU medium month, day and time; `at` is already shifted to the site time zone, so it is read as UTC.
    try {
      return new Intl.DateTimeFormat(document.documentElement.lang || 'en', {
        month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit',
        hourCycle: format.clock24 ? 'h23' : 'h12', timeZone: 'UTC',
      }).format(at);
    } catch { /* fall back to the fixed pattern below */ }
  }
  const pad = value => String(value).padStart(2, '0');
  const month = pad(at.getUTCMonth() + 1);
  const day = pad(at.getUTCDate());
  const date = format.date === 'ymd-cn' ? `${month}\u6708${day}\u65e5` : format.date === 'mdy-slash' ? `${month}/${day}` : `${month}-${day}`;
  const hour = at.getUTCHours();
  const minute = pad(at.getUTCMinutes());
  const clock = format.clock24 ? `${pad(hour)}:${minute}` : `${pad(hour % 12 || 12)}:${minute} ${hour < 12 ? 'AM' : 'PM'}`;
  return `${date} ${clock}`;
}
function renderRecent(card, items) {
  const list = card.querySelector('[data-recent-list]');
  const format = recentFormat(card);
  list.replaceChildren();
  for (const item of items) {
    const row = document.createElement('li');
    const link = document.createElement('a');
    // In a sub-path deployment, add the prefix given by <html data-base>.
    link.href = `${document.documentElement.dataset.base || ''}/thread-${item.id}`;
    const title = document.createElement('span');
    title.className = 'mini-list__title';
    title.textContent = item.title;
    const time = document.createElement('time');
    time.className = 'recent-time';
    time.dateTime = new Date(item.time).toISOString();
    time.textContent = recentTime(item.time, format);
    link.append(title, time);
    row.append(link);
    list.append(row);
  }
  if (!items.length) {
    const empty = document.createElement('li');
    empty.className = 'mini-list__empty';
    empty.textContent = t('js-recent-cleared');
    list.append(empty);
  }
  card.querySelector('[data-recent-clear]').hidden = !items.length;
}
function showRecent() {
  const card = document.querySelector('[data-recent-threads]');
  if (!card) return;
  const items = readRecent(card.dataset.recentOwner);
  // When storage cannot be read (private mode, etc.) or there are no records, the whole block stays hidden, the same
  // as without JS.
  if (!items?.length) { card.hidden = true; return; }
  renderRecent(card, items);
  card.hidden = false;
}
function syncRecent() { recordRecent(); showRecent(); }
document.addEventListener('DOMContentLoaded', syncRecent);
document.addEventListener('htmx:afterSettle', event => { if (event.detail?.target === document.body) syncRecent(); });
document.addEventListener('click', event => {
  const button = event.target instanceof Element ? event.target.closest('[data-recent-clear]') : null;
  const card = button?.closest('[data-recent-threads]');
  if (!card) return;
  writeRecent(card.dataset.recentOwner, []);
  renderRecent(card, []);
  card.querySelector('.panel__title')?.focus();
});

// Emoji zoom on hover: a per-device switch (localStorage, off by default), not stored in the database and not tied
// to the account.
// When on, emoji characters in post bodies are wrapped in span.emoji--large, leaving code and already wrapped ones
// alone; with reduced motion the CSS does not zoom. The switch appears only on the reading preferences page, and the
// whole block is hidden when storage is unavailable (private mode throws) or without JS.
const emojiZoomKey = 'gonof-emoji-zoom';
const emojiPattern = /\p{Regional_Indicator}{2}|\p{Extended_Pictographic}(?:️|\p{Emoji_Modifier})?(?:‍\p{Extended_Pictographic}(?:️|\p{Emoji_Modifier})?)*/gu;
function emojiZoomEnabled() {
  try { return localStorage.getItem(emojiZoomKey) === '1'; } catch { return false; }
}
function wrapEmoji() {
  if (!emojiZoomEnabled()) return;
  for (const content of document.querySelectorAll('.prose')) {
    const walker = document.createTreeWalker(content, NodeFilter.SHOW_TEXT, {
      acceptNode: node => node.parentElement?.closest('code, pre, script, style, .emoji--large')
        ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT,
    });
    const nodes = [];
    while (walker.nextNode()) nodes.push(walker.currentNode);
    for (const node of nodes) {
      const text = node.nodeValue || '';
      const matches = [...text.matchAll(emojiPattern)];
      if (!matches.length) continue;
      const fragment = document.createDocumentFragment();
      let last = 0;
      for (const match of matches) {
        if (match.index > last) fragment.append(text.slice(last, match.index));
        const span = document.createElement('span');
        span.className = 'emoji--large';
        span.textContent = match[0];
        fragment.append(span);
        last = match.index + match[0].length;
      }
      if (last < text.length) fragment.append(text.slice(last));
      node.replaceWith(fragment);
    }
  }
}
function showEmojiZoomSetting() {
  const panel = document.querySelector('[data-emoji-zoom-setting]');
  const toggle = panel?.querySelector('[data-emoji-zoom-toggle]');
  if (!toggle) return;
  try { localStorage.getItem(emojiZoomKey); } catch { return; }
  toggle.checked = emojiZoomEnabled();
  panel.hidden = false;
}
function syncEmojiZoom() { wrapEmoji(); showEmojiZoomSetting(); }
document.addEventListener('DOMContentLoaded', syncEmojiZoom);
document.addEventListener('htmx:afterSettle', syncEmojiZoom);
document.addEventListener('change', event => {
  const toggle = event.target instanceof Element ? event.target.closest('[data-emoji-zoom-toggle]') : null;
  if (!toggle) return;
  try {
    if (toggle.checked) localStorage.setItem(emojiZoomKey, '1');
    else localStorage.removeItem(emojiZoomKey);
  } catch { toggle.checked = false; }
});
// YouTube placeholder: the server outputs only a local placeholder and an external link, and the youtube-nocookie.com
// player is inserted only after "Load player" is clicked. The video ID is already validated by the server and is
// validated once more here; no innerHTML.
// The site's Referrer-Policy is same-origin and YouTube rejects embeds without a referrer with error 153, so the
// iframe sets strict-origin-when-cross-origin on its own, sending only this site's domain.
function showMediaButtons() {
  for (const button of document.querySelectorAll('[data-media-load]')) button.hidden = false;
}
document.addEventListener('DOMContentLoaded', showMediaButtons);
document.addEventListener('htmx:afterSettle', showMediaButtons);
document.addEventListener('click', event => {
  const button = event.target instanceof Element ? event.target.closest('[data-media-load]') : null;
  const figure = button?.closest('.video-embed');
  if (!figure) return;
  const id = figure.dataset.youtube || '';
  if (!/^[A-Za-z0-9_-]{11}$/.test(id)) return;
  const start = /^[1-9][0-9]{0,5}$/.test(figure.dataset.start || '') ? `&start=${figure.dataset.start}` : '';
  const frame = document.createElement('iframe');
  frame.className = 'video-embed__frame';
  frame.title = figure.querySelector('.video-embed__title')?.textContent || t('js-video-title');
  frame.allow = 'accelerometer; autoplay; clipboard-write; encrypted-media; gyroscope; picture-in-picture; fullscreen';
  frame.allowFullscreen = true;
  frame.referrerPolicy = 'strict-origin-when-cross-origin';
  frame.src = `https://www.youtube-nocookie.com/embed/${id}?autoplay=1${start}`;
  figure.querySelector('.video-embed__notice')?.remove();
  button.remove();
  figure.insertBefore(frame, figure.querySelector('.video-embed__actions'));
  frame.focus();
});
// The update page while a one-click upgrade runs ([data-upgrade-wait], templates/manage/updates.html) reloads every
// 3 seconds, but only once the forum answers. Between the old process exiting and systemd starting the new version
// the forum is gone for several seconds; a plain refresh in that gap lands on the reverse proxy's 502 page (or the
// browser's connection error), which never refreshes again. Without JavaScript a <noscript> meta refresh does the
// plain version.
let upgradeWait = 0;
function waitForUpgrade() {
  if (upgradeWait || !document.querySelector('[data-upgrade-wait]')) return;
  const address = location.pathname + location.search;
  const poll = async () => {
    try {
      const response = await fetch(address, { cache: 'no-store', credentials: 'same-origin', signal: AbortSignal.timeout?.(10000) });
      if (response.ok) {
        location.replace(address);
        return;
      }
    } catch {}
    upgradeWait = setTimeout(poll, 3000);
  };
  upgradeWait = setTimeout(poll, 3000);
}
document.addEventListener('DOMContentLoaded', waitForUpgrade);

// Fixed events for page scripts and skins (docs/skins.md, "Script events"), both dispatched on document and
// registered last, so they come after this file's own handlers: gonof:ready once the page has loaded, gonof:swap
// after every HTMX swap has settled, with the swapped element as detail.target (document.body after a boosted
// navigation).
document.addEventListener('DOMContentLoaded', () => document.dispatchEvent(new CustomEvent('gonof:ready')));
document.addEventListener('htmx:afterSettle', event => {
  document.dispatchEvent(new CustomEvent('gonof:swap', { detail: { target: event.detail?.target ?? null } }));
});
})();
