// The autohide-header behavior module (docs/skins.md, "Behavior modules"): the sticky header slides away while the
// reader scrolls down and comes back as soon as they scroll up, giving long pages the whole window. While it is away,
// <html data-header-hidden> is set and --sticky-top drops to the safe area, so sticky asides, the side navigation,
// anchor targets and the scripts that read the header height follow. It never hides while focus or an open menu is in
// the header, and keyboard focus moving into the header brings it back. With prefers-reduced-motion it hides and
// shows without sliding (site.css switches transitions off).
(() => {
  if (window.gonofBehaviorAutohideHeader) return;
  window.gonofBehaviorAutohideHeader = true;
  const root = document.documentElement;
  const enabled = () => (root.dataset.behaviors || '').split(' ').includes('autohide-header');
  // Scrolling less than this either way changes nothing, so a resting finger or a tremor does not flicker it.
  const SLACK = 8;
  let last = scrollY;
  let frame = 0;

  const header = () => document.querySelector('.masthead');
  function busy() {
    const masthead = header();
    if (!masthead) return true;
    return masthead.contains(document.activeElement) || !!masthead.querySelector('details[open]');
  }
  function show() {
    if (root.hasAttribute('data-header-hidden')) root.removeAttribute('data-header-hidden');
  }
  function update() {
    frame = 0;
    const y = scrollY;
    const delta = y - last;
    if (!enabled() || busy()) { show(); last = y; return; }
    // Near the top the header always shows; it only goes once the page has moved past two header heights.
    const height = header()?.getBoundingClientRect().height || 0;
    if (y <= height * 2) { show(); last = y; return; }
    if (Math.abs(delta) < SLACK) return;
    if (delta > 0) root.setAttribute('data-header-hidden', '');
    else show();
    last = y;
  }

  addEventListener('scroll', () => { if (!frame) frame = requestAnimationFrame(update); }, { passive: true });
  document.addEventListener('focusin', event => { if (header()?.contains(event.target)) show(); });
  document.addEventListener('toggle', event => { if (event.target instanceof Element && header()?.contains(event.target)) show(); }, true);
  // A boosted navigation starts the new page at the top with the header showing.
  document.addEventListener('gonof:swap', event => { if (event.detail?.target === document.body) { show(); last = scrollY; } });
})();
