// Visual checks on every listed page, desktop and phone, light and dark: no horizontal scrolling, no errors in the
// console, a visible keyboard focus, and, on the English site, no Chinese text anywhere on the page or in its labels
// (the only exceptions are the Simplified Chinese name of that language on the language pickers and the sample of the
// Chinese date format in the admin area).
import { expect, test } from '@playwright/test';
import { instances, pages, pathOf } from './support/pages.js';

const HAN = /\p{Script=Han}/u;
// The language's own name and the year, month and day signs of the `ymd-cn` date format.
const ALLOWED = [/\u7b80\u4f53\u4e2d\u6587/g, /\d{4}\u5e74\d{1,2}\u6708\d{1,2}\u65e5/g];

for (const instance of instances()) {
  for (const who of ['anonymous', 'admin']) {
    test.describe(`checks ${instance.language} ${who}`, () => {
      if (who === 'admin') test.use({ storageState: instance.adminState });
      for (const entry of pages.filter((entry) => entry.who === who)) {
        test(`${instance.language} ${entry.name}`, async ({ page }) => {
          const problems = [];
          page.on('console', (message) => {
            // A page that is meant to answer 404 logs its own status; that is not a problem.
            if (entry.status && message.location().url.startsWith(`${instance.origin}${pathOf(entry, instance)}`)) return;
            if (message.type() === 'error') problems.push(`console: ${message.text()} (${message.location().url})`);
          });
          page.on('pageerror', (error) => problems.push(`page error: ${error.message}`));
          const response = await page.goto(`${instance.origin}${pathOf(entry, instance)}`, { waitUntil: 'networkidle' });
          expect(response.status()).toBe(entry.status || 200);

          const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
          expect(overflow, 'the page scrolls sideways').toBeLessThanOrEqual(1);

          if (instance.language === 'en') {
            const text = await page.evaluate(() => {
              const labels = [...document.querySelectorAll('[aria-label], [title], [placeholder], [alt]')].flatMap((element) =>
                ['aria-label', 'title', 'placeholder', 'alt'].map((name) => element.getAttribute(name) || ''));
              return [document.title, document.body.innerText, ...labels].join('\n');
            });
            let rest = text;
            for (const allowed of ALLOWED) rest = rest.replace(allowed, '');
            const lines = rest.split('\n').filter((line) => HAN.test(line));
            expect(lines, 'Chinese text on an English page').toEqual([]);
          }

          // Keyboard focus: the first few Tab stops must each show an outline or a ring, or, on a form field, a
          // darker edge: its own border or its wrapper's, or the line drawn around a checkbox or radio
          // (docs/styles.md, "HTML conventions"); the header search's button changes its ground in the box.
          const edges = (element) => [element, element.parentElement].flatMap((node) => {
            const style = node && getComputedStyle(node);
            return style ? [style.borderColor, style.filter, style.backgroundColor] : [];
          });
          for (let step = 0; step < 4; step++) {
            await page.keyboard.press('Tab');
            const focus = await page.evaluate(() => {
              const element = document.activeElement;
              if (!element || element === document.body) return null;
              const rect = element.getBoundingClientRect();
              // The ring may sit on the element or, through :focus-within, on its field wrapper.
              const shows = (node) => {
                const style = getComputedStyle(node);
                return (style.outlineStyle !== 'none' && parseFloat(style.outlineWidth) > 0) || style.boxShadow !== 'none';
              };
              const wrapper = element.parentElement;
              return {
                name: `${element.tagName.toLowerCase()}.${[...element.classList].join('.')}`,
                // The header search's button shows focus like its field: the search box darkens its edge
                // (docs/styles.md, "HTML conventions"), and the button its ground.
                field: element.matches('input, select, textarea, .masthead__search > button'),
                shown: shows(element) || Boolean(wrapper && wrapper.matches(':focus-within') && shows(wrapper)),
                visible: rect.width > 0 && rect.height > 0,
              };
            });
            if (!focus) break;
            expect(focus.visible, `${focus.name} has focus but no size`).toBe(true);
            let shown = focus.shown;
            if (!shown && focus.field) {
              // Compare the field's edge, and its wrapper's, with how they look once focus has moved back a stop,
              // then return to the field so the Tab order stays in step.
              const field = await page.evaluateHandle(() => document.activeElement);
              const focused = await field.evaluate(edges);
              await page.keyboard.press('Shift+Tab');
              const resting = await field.evaluate(edges);
              await page.keyboard.press('Tab');
              const back = await field.evaluate((element) => element === document.activeElement);
              expect(back, `${focus.name} lost its Tab stop`).toBe(true);
              shown = focused.some((value, index) => value !== resting[index]);
              await field.dispose();
            }
            expect(shown, `${focus.name} shows no focus`).toBe(true);
          }
          expect(problems).toEqual([]);
        });
      }
    });
  }
}
