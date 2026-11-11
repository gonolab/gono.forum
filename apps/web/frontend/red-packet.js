import { prizeKinds } from './lottery-prizes.js';

const t = (key, args) => (window.gonofT ? window.gonofT(key, args) : key);

// An equal-split red packet must divide evenly when published; a random red packet only requires at least 1 credit
// per share.
export function bindPacketPreview(form, signal) {
  const output = form.querySelector('[data-packet-preview]');
  if (!output) return;
  const { budget, shares, distribution, category } = form.elements;
  const instructions = output.textContent;
  const credit = prizeKinds(form).credit;
  function update() {
    const total = budget.valueAsNumber;
    const count = shares.valueAsNumber;
    let text = instructions;
    let error = '';
    if (category.value === '2' && Number.isSafeInteger(total) && total > 0 && Number.isSafeInteger(count) && count > 0) {
      // Amounts go in as text: written as typed, without digit grouping.
      const amounts = { total: String(total), count: String(count), each: String(total / count), credit };
      if (count > total) error = t('js-packet-too-many', amounts);
      else if (distribution.value === 'random') text = t('js-packet-random', amounts);
      else if (total % count) error = t('js-packet-indivisible', amounts);
      else text = t('js-packet-equal', amounts);
    }
    shares.setCustomValidity(error);
    if (error) shares.setAttribute('aria-invalid', 'true');
    else shares.removeAttribute('aria-invalid');
    output.toggleAttribute('data-invalid', Boolean(error));
    output.textContent = error || text;
  }
  update();
  form.addEventListener('input', event => {
    if (['category', 'distribution', 'budget', 'shares'].includes(event.target.name)) update();
  }, { signal });
}
