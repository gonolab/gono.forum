// Text areas whose limit is in UTF-8 bytes (`data-max-bytes`, apps/web/static/js/site.js): the policy and mail
// template forms. `maxlength` counts characters, so Chinese text could pass it at three times the bytes the server
// takes; the browser checks the bytes and does not submit while the text is over.
import { expect, test } from '@playwright/test';
import { instances } from './support/pages.js';

// U+6587, a Chinese character of three UTF-8 bytes.
const HAN = '文';

for (const instance of instances().filter((entry) => entry.language === 'en')) {
  test.describe('byte limits', () => {
    test.use({ storageState: instance.adminState });
    test('policy and mail template bodies are limited by bytes in the browser', async ({ page }, testInfo) => {
      test.skip(testInfo.project.name !== 'desktop-light', 'behavior, not layout: one viewport is enough');
      for (const [path, max] of [['/manage/policies/terms', 60000], ['/manage/mail/templates/content', 16000]]) {
        await page.goto(`${instance.origin}${path}`);
        const body = page.locator('textarea[name="body"]');
        await expect(body).toHaveAttribute('data-max-bytes', String(max));
        // As many characters as fit, then one more: within any character limit of that size, over the byte limit.
        const fits = Math.floor(max / 3);
        await body.fill(HAN.repeat(fits + 1));
        const over = await body.evaluate((element) => ({ valid: element.validity.valid, message: element.validationMessage }));
        expect(over.valid, path).toBe(false);
        expect(over.message, path).toContain(`${(fits + 1) * 3} bytes`);
        await body.fill(HAN.repeat(fits));
        expect(await body.evaluate((element) => element.validity.valid), path).toBe(true);
      }
    });
  });
}
