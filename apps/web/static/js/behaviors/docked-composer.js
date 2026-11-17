// The docked-composer behavior module (docs/skins.md, "Behavior modules"): on thread pages where the reader can
// reply, the reply box leaves the end of the page and docks at the bottom of the window, behind a bar with a button
// that opens and closes it, so a reply can be written while reading any floor. Without it the reply box stays inline at
// the end of the page. <html data-composer-dock> says whether it is "open" or "closed"; the reply box keeps its place
// in the markup, so the editor (composer.js) and the forms work as before. Opening focuses the text field; Escape in
// the box or the button closes it and gives the button focus back. Links to the reply box on the page (the thread
// head's Reply) open it, a page opened at #reply-editor (Reply and Quote on a floor) starts with it open, and a
// published reply closes it.
(() => {
  if (window.gonofBehaviorDockedComposer) return;
  window.gonofBehaviorDockedComposer = true;
  const root = document.documentElement;
  const t = (key, args) => (window.gonofText ? window.gonofText(key, args) : key);
  const enabled = () => (root.dataset.behaviors || '').split(' ').includes('docked-composer');
  let dock = null;
  let started = null;

  const box = () => document.getElementById('reply-editor');
  const field = () => box()?.querySelector('textarea');

  function setOpen(open, focus) {
    if (!dock) return;
    root.setAttribute('data-composer-dock', open ? 'open' : 'closed');
    dock.button.setAttribute('aria-expanded', String(open));
    dock.label.textContent = open ? t('js-dock-close') : t('js-dock-open');
    if (open && focus) field()?.focus();
    if (!open && focus) dock.button.focus();
  }

  function stop() {
    dock?.bar.remove();
    dock = null;
    root.removeAttribute('data-composer-dock');
  }

  function start() {
    if (started === document.body) return;
    started = document.body;
    stop();
    // Only where the reply box holds the editor: locked threads and visitors who cannot reply keep the notice inline.
    if (!enabled() || !box()?.querySelector('form[data-composer] textarea')) return;
    const bar = document.createElement('div');
    bar.className = 'composer-dock';
    bar.dataset.composerDock = '';
    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'button composer-dock__toggle';
    button.setAttribute('aria-controls', 'reply-editor');
    const label = document.createElement('span');
    button.append(label);
    bar.append(button);
    document.body.append(bar);
    dock = { bar, button, label };
    button.addEventListener('click', () => setOpen(root.getAttribute('data-composer-dock') !== 'open', true));
    // Opened at the reply box (a floor's Reply or Quote), or with a reply or quote under way: start open.
    const context = !!box().querySelector('.editor__context') || location.hash === '#reply-editor';
    setOpen(context, false);
  }

  document.addEventListener('click', event => {
    if (!dock || event.defaultPrevented || event.button !== 0) return;
    const link = event.target instanceof Element ? event.target.closest('a[href]') : null;
    if (!link || link.closest('#reply-editor')) return;
    const url = new URL(link.href, location.href);
    if (url.hash !== '#reply-editor' || url.pathname !== location.pathname || url.search !== location.search) return;
    event.preventDefault();
    setOpen(true, true);
  });
  document.addEventListener('keydown', event => {
    if (!dock || event.key !== 'Escape' || event.defaultPrevented || root.getAttribute('data-composer-dock') !== 'open') return;
    const target = event.target instanceof Element ? event.target : null;
    // The editor's own menus (emoji, mentions) and full screen handle Escape first.
    if (!target || root.classList.contains('has-write-fullscreen')) return;
    if (target.closest('#reply-editor') || target.closest('[data-composer-dock]')) setOpen(false, true);
  });
  addEventListener('hashchange', () => { if (dock && location.hash === '#reply-editor') setOpen(true, true); });
  // The server says a reply was published after swapping in the new floors (posting.rs, HX-Trigger-After-Swap).
  document.addEventListener('composer:published', () => setOpen(false, false));
  document.addEventListener('gonof:ready', start);
  document.addEventListener('gonof:swap', event => { if (event.detail?.target === document.body) { started = null; start(); } });
  if (document.readyState !== 'loading') start();
})();
