const t = (key, args) => (window.gonofT ? window.gonofT(key, args) : key);

// datetime-local shows the site's wall-clock time. Its string has no time zone, so it must not be handed to the
// browser to parse in the local time zone.
export function enhanceDeadline(form) {
  const panel = form.querySelector('[data-activity-deadline]');
  if (!panel) return;
  const field = form.elements.deadline;
  const minutes = Number.parseInt(field.dataset.offsetMinutes ?? '', 10);
  if (!Number.isInteger(minutes)) return;
  const offset = minutes * 60000;
  const choices = panel.querySelector('[data-deadline-choices]');
  const note = panel.querySelector('[data-deadline-note]');
  const presets = form.elements.deadline_preset;
  const day = 86400000;
  const wallTime = (now, days) => new Date(now + days * day + offset).toISOString().slice(0, 16);
  const instant = () => field.value ? Date.parse(`${field.value}Z`) - offset : NaN;

  let customInput = false;
  function readDeadline() {
    if (field.disabled) return;
    const now = Date.now();
    let at = instant();
    // An old draft may already have expired; if server-side validation failed, keep it as is so the user edits the
    // value they see.
    if ((!Number.isFinite(at) || at <= now) && !customInput && form.dataset.restoreDraft !== 'false') {
      if (field.value) note.textContent = t('js-deadline-expired');
      field.value = wallTime(now, 3);
      at = instant();
    }
    // A draft stores a concrete instant, so restoring it does not recount the period from today. datetime-local is
    // only precise to the minute.
    const days = [1, 3, 7].find(value => Math.abs(at - now - value * day) < 60000);
    presets.value = days && !customInput ? String(days) : 'custom';
  }

  choices.hidden = false;
  readDeadline();
  form.addEventListener('change', event => {
    if (event.target.name === 'category') readDeadline();
  });
  choices.addEventListener('change', () => {
    note.textContent = '';
    customInput = presets.value === 'custom';
    if (customInput) {
      field.focus();
      return;
    }
    field.value = wallTime(Date.now(), Number(presets.value));
  });
  field.addEventListener('input', () => {
    customInput = true;
    presets.value = 'custom';
    note.textContent = '';
  });
}
