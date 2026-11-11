(() => {
  if (window.gonofDebugInitialized) return;
  window.gonofDebugInitialized = true;
  // Messages come from js.ftl through the page's language script, loaded before this one.
  const text = name => (window.gonofT ? window.gonofT(name) : name);

  function revealCopyButtons() {
    for (const button of document.querySelectorAll('#gonof-debug [data-debug-copy]')) button.hidden = false;
  }

  function selectSql(code) {
    const selection = window.getSelection();
    if (!selection) return;
    const range = document.createRange();
    range.selectNodeContents(code);
    selection.removeAllRanges();
    selection.addRange(range);
  }

  document.addEventListener('click', async event => {
    if (!(event.target instanceof Element)) return;
    const button = event.target.closest('#gonof-debug [data-debug-copy]');
    if (!button || button.disabled) return;
    const query = button.closest('[data-debug-query]');
    const code = query?.querySelector('[data-debug-sql]');
    const status = query?.querySelector('[data-debug-copy-status]');
    if (!code || !status) return;
    button.disabled = true;
    try {
      if (!navigator.clipboard?.writeText) throw new Error('Clipboard unavailable');
      await navigator.clipboard.writeText(code.textContent);
      status.textContent = text('js-debug-copied');
    } catch {
      selectSql(code);
      status.textContent = text('js-debug-copy-selected');
    } finally {
      button.disabled = false;
    }
  });

  revealCopyButtons();
  document.addEventListener('DOMContentLoaded', revealCopyButtons);
  document.addEventListener('htmx:afterSwap', revealCopyButtons);
})();
