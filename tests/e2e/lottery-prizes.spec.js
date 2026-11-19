// The lottery prize editor (apps/web/frontend/lottery-prizes.js) writes each row into the `prizes` field as
// `name | winners | type | points each | note`. The server reads the third column as the prize type, so the editor
// writes the identifiers `credit`, `code` and `item` there, never the site's name for its points: a name such as
// "item", or one containing "|", would otherwise turn a points prize into another type or change its amount.
import { expect, test } from '@playwright/test';
import { instances } from './support/pages.js';

for (const instance of instances()) {
  test.describe(`lottery prizes (${instance.language})`, () => {
    test.use({ storageState: instance.adminState });
    test('the prize editor writes type identifiers, not the points name', async ({ page }, testInfo) => {
      test.skip(testInfo.project.name !== 'desktop-light', 'behavior, not layout: one viewport is enough');
      await page.goto(`${instance.origin}/new`);
      await page.locator('input[name="category"][value="3"]').check();
      const add = page.locator('[data-add-prize]');
      await add.click();
      const rows = page.locator('[data-prize-rows] > fieldset');
      const points = rows.nth(0);
      await points.locator('[data-prize-name]').fill('Grand prize');
      await points.locator('[data-prize-winners]').fill('2');
      await points.locator('[data-prize-amount]').fill('50');
      // The radio shows the site's name for its points, which is not the identifier the field must carry.
      const shown = await points.locator('[data-prize-kind] input[value="credit"]').evaluate((radio) => radio.parentElement.textContent.trim());
      expect(shown).not.toBe('credit');
      const prizes = page.locator('textarea[name="prizes"]');
      await expect(prizes).toHaveValue('Grand prize | 2 | credit | 50 | ');
      await add.click();
      const item = rows.nth(1);
      await item.locator('[data-prize-name]').fill('Mug');
      await item.locator('[data-prize-kind] input[value="item"]').check();
      await expect(prizes).toHaveValue('Grand prize | 2 | credit | 50 | \nMug | 1 | item |  | ');
    });
  });
}
