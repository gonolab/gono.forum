// The built-in skins (docs/skins.md, "Built-in skins") with the behavior modules they turn on, on the development
// fixture. Each test previews its skin, which shows it to this browser context only, so other tests keep the site's
// skin. The fixture's thread 104 is fixed and the administrator may reply there.
import { expect, test } from '@playwright/test';
import { instances } from './support/pages.js';

/** Starts a preview of `skin` in the page's browser context (the preview cookie). */
async function preview(page, instance, skin) {
  const manage = await (await page.request.get(`${instance.origin}/manage/skins`)).text();
  const csrf = manage.match(/name="_csrf" value="([^"]+)"/)[1];
  const answer = await page.request.post(`${instance.origin}/manage/skins/preview`, {
    headers: { origin: instance.origin },
    form: { _csrf: csrf, choice: `${skin}:` },
    maxRedirects: 0,
  });
  expect(answer.status()).toBe(303);
}

for (const instance of instances().filter((instance) => instance.content === 'fixture')) {
  test.describe(`built-in skins (${instance.language})`, () => {
    test.use({ storageState: instance.adminState });

    for (const skin of ['atrium', 'harbor', 'ledger']) {
      test(`${skin}: the reply box docked open covers the page under it`, async ({ page }) => {
        await preview(page, instance, skin);
        await page.goto(`${instance.origin}/thread-104`);
        const html = page.locator('html');
        await expect(html).toHaveAttribute('data-skin', skin);
        await page.locator('div[data-composer-dock] > button').click();
        await expect(html).toHaveAttribute('data-composer-dock', 'open');
        const box = page.locator('#reply-editor');
        await expect(box).toBeVisible();
        // The skin's own look for the reply box at the end of the thread (no ground, no padding, a dashed outline)
        // must not reach the floating panel: it keeps an opaque ground, so the page text under it does not show.
        const look = await box.evaluate((element) => {
          const style = getComputedStyle(element);
          return { background: style.backgroundColor, position: style.position, padding: style.paddingInlineStart };
        });
        expect(look.position).toBe('fixed');
        expect(look.background, 'an opaque ground').toMatch(/^rgb\(/);
        expect(look.padding).toBe('16px');
        // What is drawn at the box's title is the box itself.
        const onTop = await page.locator('#reply-editor-title').evaluate((title) => {
          const area = title.getBoundingClientRect();
          const found = document.elementFromPoint(area.left + 4, area.top + area.height / 2);
          return title.closest('#reply-editor').contains(found);
        });
        expect(onTop).toBe(true);
      });
    }
  });
}
