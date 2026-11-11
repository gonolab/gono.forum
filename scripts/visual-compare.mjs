// Builds a side-by-side page from two screenshot sets taken by tests/e2e/screens.spec.js, for judging a visual change
// by eye (no pixel thresholds). Usage: node scripts/visual-compare.mjs before after → .local/visual/compare-before-after.html
import { existsSync, readdirSync, writeFileSync } from 'node:fs';
import { join, relative } from 'node:path';

const [, , left = 'before', right = 'after'] = process.argv;
const base = join(process.cwd(), '.local/visual');
const target = join(base, `compare-${left}-${right}.html`);
const rows = [];
for (const language of readdirSync(join(base, left)).sort()) {
  for (const project of readdirSync(join(base, left, language)).sort()) {
    for (const file of readdirSync(join(base, left, language, project)).sort()) {
      const a = join(base, left, language, project, file);
      const b = join(base, right, language, project, file);
      rows.push({ language, project, page: file.replace(/\.png$/, ''), a: relative(base, a), b: existsSync(b) ? relative(base, b) : null });
    }
  }
}
const escape = (text) => text.replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' })[c]);
const options = (values) => [...new Set(values)].map((value) => `<option>${escape(value)}</option>`).join('');
const html = `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>Visual comparison</title>
<style>
:root { color-scheme: light dark; --bg: #f7f8fb; --panel: #fff; --text: #151b2b; --muted: #606b7d; --line: #e0e4eb; }
@media (prefers-color-scheme: dark) { :root { --bg: #0a0f1d; --panel: #121827; --text: #f1f5f9; --muted: #94a3b8; --line: #263045; } }
body { margin: 0; background: var(--bg); color: var(--text); font: 14px/1.5 system-ui, sans-serif; }
header { position: sticky; top: 0; z-index: 1; display: flex; flex-wrap: wrap; gap: 12px; align-items: center; padding: 10px 16px; background: var(--panel); border-bottom: 1px solid var(--line); }
header h1 { margin: 0; font-size: 16px; }
select, label { font: inherit; }
section { margin: 16px; padding: 12px; background: var(--panel); border: 1px solid var(--line); border-radius: 12px; }
section h2 { margin: 0 0 8px; font-size: 14px; }
.pair { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; align-items: start; }
.pair.stack { grid-template-columns: 1fr; }
figure { margin: 0; }
figcaption { color: var(--muted); font-size: 12px; margin-bottom: 4px; }
img { display: block; max-width: 100%; height: auto; border: 1px solid var(--line); }
.missing { color: var(--muted); padding: 24px; border: 1px dashed var(--line); }
</style></head><body>
<header><h1>${escape(left)} → ${escape(right)}</h1>
<label>Language <select data-filter="language"><option value="">all</option>${options(rows.map((row) => row.language))}</select></label>
<label>Viewport <select data-filter="project"><option value="">all</option>${options(rows.map((row) => row.project))}</select></label>
<label>Page <select data-filter="page"><option value="">all</option>${options(rows.map((row) => row.page))}</select></label>
<label><input type="checkbox" data-stack> one above the other</label>
<span>${rows.length} pairs</span></header>
${rows.map((row) => `<section data-language="${escape(row.language)}" data-project="${escape(row.project)}" data-page="${escape(row.page)}">
<h2>${escape(row.language)} · ${escape(row.project)} · ${escape(row.page)}</h2>
<div class="pair"><figure><figcaption>${escape(left)}</figcaption><img loading="lazy" src="${escape(row.a)}" alt=""></figure>
<figure><figcaption>${escape(right)}</figcaption>${row.b ? `<img loading="lazy" src="${escape(row.b)}" alt="">` : '<div class="missing">missing</div>'}</figure></div></section>`).join('\n')}
<script>
const filters = [...document.querySelectorAll('[data-filter]')];
const apply = () => {
  for (const section of document.querySelectorAll('section')) {
    section.hidden = filters.some((select) => select.value && section.dataset[select.dataset.filter] !== select.value);
  }
};
for (const select of filters) select.addEventListener('change', apply);
document.querySelector('[data-stack]').addEventListener('change', (event) => {
  for (const pair of document.querySelectorAll('.pair')) pair.classList.toggle('stack', event.target.checked);
});
</script></body></html>
`;
writeFileSync(target, html);
console.log(`${rows.length} pairs; open ${target}`);
