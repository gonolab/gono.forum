const t = (key, args) => (window.gonofT ? window.gonofT(key, args) : key);

// Publishing, preview and drafts still read options and max_choices. Without scripts these two native fields are
// filled in directly.
export function enhancePoll(form) {
  const panel = form.querySelector('[data-poll]');
  if (!panel) return;
  const source = form.elements.options;
  const limit = form.elements.max_choices;
  const mode = panel.querySelector('[data-poll-mode]');
  const list = panel.querySelector('[data-poll-list]');
  const add = panel.querySelector('[data-poll-add]');
  const status = panel.querySelector('[data-poll-status]');
  const maxOptions = 20;

  function row(value) {
    const container = panel.querySelector('[data-poll-row]').content.firstElementChild.cloneNode(true);
    container.querySelector('input').value = value;
    return container;
  }

  function sync() {
    const rows = [...list.children];
    const values = rows.map(item => item.querySelector('input').value);
    const filled = values.filter(value => value.trim()).length;
    source.value = values.join('\n').replace(/\n+$/, '');
    rows.forEach((item, index) => {
      item.querySelector('input').disabled = source.disabled;
      const label = t('js-poll-option', { n: index + 1 });
      item.querySelector('span').textContent = label;
      const remove = item.querySelector('button');
      remove.setAttribute('aria-label', t('js-poll-remove', { option: label }));
      remove.disabled = source.disabled || rows.length <= 2;
    });
    add.disabled = source.disabled || rows.length >= maxOptions;
    const multiple = form.elements.poll_mode.value === 'multiple';
    limit.closest('label').hidden = !multiple;
    limit.min = multiple ? '2' : '1';
    limit.max = String(Math.max(2, filled));
    if (!multiple) limit.value = '1';
    list.querySelector('input').setCustomValidity(filled < 2 ? t('js-poll-min') : '');
  }

  let savedMaximum = Number(limit.value) || 2;
  const multiple = savedMaximum > 1;
  form.elements.poll_mode.value = multiple ? 'multiple' : 'single';
  const values = source.value ? source.value.split(/\r?\n/) : [];
  while (values.length < 2) values.push('');
  list.append(...values.map(row));
  mode.hidden = false;
  panel.querySelector('[data-poll-rows]').hidden = false;
  panel.querySelector('[data-poll-source]').hidden = true;
  panel.querySelector('[data-poll-limit] span').textContent = t('js-poll-max');
  sync();

  form.addEventListener('change', event => {
    if (event.target.name === 'category') sync();
  });
  mode.addEventListener('change', () => {
    if (form.elements.poll_mode.value === 'single') savedMaximum = Number(limit.value) || 2;
    else limit.value = String(Math.min(Number(limit.max), Math.max(2, savedMaximum)));
    sync();
  });

  list.addEventListener('input', sync);
  add.addEventListener('click', () => {
    if (list.children.length >= maxOptions) return;
    const next = row('');
    list.append(next);
    sync();
    next.querySelector('input').focus();
    source.dispatchEvent(new Event('input', { bubbles: true }));
  });
  list.addEventListener('click', event => {
    const remove = event.target.closest('button');
    if (!remove || list.children.length <= 2) return;
    const item = remove.closest('.poll-setup__option');
    const neighbor = item.nextElementSibling || item.previousElementSibling;
    item.remove();
    sync();
    if (limit.valueAsNumber > Number(limit.max)) limit.value = limit.max;
    neighbor.querySelector('input').focus();
    source.dispatchEvent(new Event('input', { bubbles: true }));
  });
  list.addEventListener('paste', event => {
    const input = event.target;
    const text = event.clipboardData?.getData('text/plain') || '';
    if (!(input instanceof HTMLInputElement) || !/[\r\n]/.test(text)) return;
    event.preventDefault();
    const lines = (input.value.slice(0, input.selectionStart) + text + input.value.slice(input.selectionEnd))
      .split(/\r\n?|\n/).filter(value => value.trim());
    if (!lines.length) return;
    const item = input.closest('.poll-setup__option');
    const following = [];
    let next = item.nextElementSibling;
    while (next && !next.querySelector('input').value && following.length < lines.length - 1) {
      following.push(next);
      next = next.nextElementSibling;
    }
    if (list.children.length - following.length + lines.length - 1 > maxOptions) {
      status.textContent = t('js-poll-too-many');
      return;
    }
    for (const empty of following) empty.remove();
    status.textContent = '';
    input.value = lines[0];
    const added = lines.slice(1).map(row);
    item.after(...added);
    sync();
    (added.at(-1) || item).querySelector('input').focus();
    source.dispatchEvent(new Event('input', { bubbles: true }));
  });
}
