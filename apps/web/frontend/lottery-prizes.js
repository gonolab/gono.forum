// The web page and the no-script form share the two-column format of parse_prize_lines. Redemption codes are sent to
// the server separately, and are written neither into prizes (which goes into the local draft) nor into DOM
// attributes or error messages.
const t = (key, args) => (window.gonofT ? window.gonofT(key, args) : key);
const lines = text => text.split(/\r?\n/).map(line => line.trim()).filter(Boolean);

// The words the server accepts in the type column, as the page writes them on the prize type radios: the site's name
// for its points and this language's words for codes and other prizes (editor-prize-kind-*). They are read back from
// drafts and from text typed into the no-script form; the editor itself writes the identifiers (credit, code, item),
// which the server reads the same way whatever the site calls its points.
export function prizeKinds(form) {
  const words = { credit: 'credit', code: 'code', item: 'item' };
  const template = form.querySelector('[data-lottery-prizes] template');
  for (const radio of template?.content.querySelectorAll('[data-prize-kind] input') || []) {
    const text = radio.parentElement.textContent.trim();
    if (text && radio.value in words) words[radio.value] = text;
  }
  return words;
}

function readPrizes(text, codeText, max, kinds) {
  const entries = [];
  const codes = lines(codeText);
  for (const line of lines(text)) {
    const fields = line.split('|');
    const [name, winners, kindText, amount = ''] = fields.slice(0, 4).map(field => field.trim());
    const note = fields.slice(4).join('|').trim();
    const kind = Object.keys(kinds).find(key => key === kindText || kinds[key] === kindText);
    // An old draft or echoed error that cannot be filled into the controls losslessly keeps the raw two columns for
    // the author to fix.
    if (!kind || entries.length >= max || !/^\d*$/.test(winners) || !/^\d*$/.test(amount)) return null;
    if (kind !== 'credit' && amount && amount !== '0') return null;
    entries.push({ name, winners, kind, amount, note, codes: '' });
  }
  if (codes.length) {
    const slots = entries.filter(entry => entry.kind === 'code');
    if (slots.reduce((sum, entry) => sum + Number(entry.winners), 0) !== codes.length) return null;
    for (const entry of slots) entry.codes = codes.splice(0, Number(entry.winners)).join('\n');
  }
  return entries;
}

function nameError(value) {
  if (value.includes('|')) return t('js-prize-name-pipe');
  if (!value.trim() || [...value.trim()].length > 60) return t('js-prize-name-length');
  return '';
}

function codeError(codes, slots, seen) {
  let duplicate = false;
  for (const code of codes) {
    if (seen.has(code)) duplicate = true;
    seen.add(code);
  }
  if (codes.length !== slots) return t('js-prize-code-count');
  if (duplicate) return t('js-prize-code-duplicate');
  if (codes.some(value => !/^[\x20-\x7e]{1,255}$/.test(value) || /["\\]/.test(value))) {
    return t('js-prize-code-format');
  }
  return '';
}

export function enhanceLotteryPrizes(form, signal) {
  const section = form.querySelector('[data-lottery-prizes]');
  if (!section) return;
  const source = form.elements.prizes;
  const codeSource = form.elements.lottery_codes;
  const max = Number(section.dataset.maxPrizes);
  const kinds = prizeKinds(form);
  const entries = readPrizes(source.value, codeSource.value, max, kinds);
  if (!entries) return;

  const rows = section.querySelector('[data-prize-rows]');
  const add = section.querySelector('[data-add-prize]');
  const template = section.querySelector('template');
  let groupNumber = 0;
  function append(entry = { name: '', winners: '1', kind: 'credit', amount: '', codes: '', note: '' }) {
    const row = template.content.firstElementChild.cloneNode(true);
    const group = `prize_kind_${++groupNumber}`;
    for (const radio of row.querySelectorAll('[data-prize-kind] input')) {
      radio.name = group;
      radio.checked = radio.value === entry.kind;
    }
    for (const field of ['name', 'winners', 'amount', 'codes', 'note']) row.querySelector(`[data-prize-${field}]`).value = entry[field];
    rows.append(row);
    return row;
  }

  function sync(rewrite = true) {
    const prizes = [];
    const allCodes = [];
    const seenCodes = new Set();
    const lottery = form.elements.category.value === '3';
    for (const [index, row] of [...rows.children].entries()) {
      // On the first restore the row controls are inserted after the generic category switch; a hidden category
      // must not take part in form validation.
      row.disabled = !lottery;
      const control = name => row.querySelector(`[data-prize-${name}]`);
      const name = control('name'), winners = control('winners'), kind = control('kind').querySelector('input:checked').value;
      const amount = control('amount'), codes = control('codes'), note = control('note');
      row.querySelector('legend').textContent = t('js-prize-legend', { n: index + 1 });
      row.querySelector('[data-remove-prize]').setAttribute('aria-label', t('js-prize-remove', { n: index + 1 }));
      amount.closest('label').hidden = kind !== 'credit';
      codes.closest('label').hidden = kind !== 'code';
      amount.disabled = !lottery || kind !== 'credit';
      codes.disabled = !lottery || kind !== 'code';
      name.setCustomValidity(nameError(name.value));
      note.setCustomValidity([...note.value.trim()].length > 200 ? t('js-prize-note-long') : '');
      if (kind === 'code') {
        const values = lines(codes.value);
        codes.setCustomValidity(codeError(values, Number(winners.value), seenCodes));
        allCodes.push(...values);
      }
      prizes.push([name.value, winners.value, kind, kind === 'credit' ? amount.value : '', note.value].join(' | '));
    }

    if (rewrite) {
      source.value = prizes.join('\n');
      codeSource.value = allCodes.join('\n');
    }
    const custom = prizes.length > 0;
    form.toggleAttribute('data-custom-lottery', lottery && custom);
    // The budget and shares of other categories, and the disabled state of all prize controls under a category, are
    // handled by the generic category switch.
    if (lottery) {
      form.elements.budget.disabled = custom;
      form.elements.shares.disabled = custom;
    }
    add.disabled = prizes.length >= max;
    add.textContent = custom ? t('js-prize-add') : t('js-prize-add-custom');
    section.querySelector('[data-prize-mode]').textContent = custom
      ? t('js-prize-mode-custom', { credit: kinds.credit })
      : t('js-prize-mode-simple', { credit: kinds.credit });
  }

  for (const entry of entries) append(entry);
  section.querySelector('[data-prize-fallback]').hidden = true;
  section.querySelector('[data-prize-editor]').hidden = false;
  // The draft has already been restored, so sync to the selected category first. The raw format is rewritten only
  // after the author edits.
  sync(false);

  // The radio group names are only for keyboard navigation within each row; what is submitted to the server is still
  // the two fields prizes and lottery_codes.
  form.addEventListener('formdata', event => {
    for (const radio of rows.querySelectorAll('[data-prize-kind] :checked')) event.formData.delete(radio.name);
  }, { signal });
  rows.addEventListener('input', () => sync(), { signal });
  rows.addEventListener('change', () => sync(), { signal });
  section.addEventListener('click', event => {
    const remove = event.target.closest('[data-remove-prize]');
    if (!remove && event.target !== add) return;
    let focus;
    if (remove) {
      const row = remove.closest('fieldset');
      focus = (row.nextElementSibling || row.previousElementSibling)?.querySelector('[data-remove-prize]') || add;
      row.remove();
    } else {
      focus = append().querySelector('[data-prize-name]');
    }
    sync();
    source.dispatchEvent(new Event('input', { bubbles: true }));
    focus.focus();
  }, { signal });
  return () => sync(false);
}
