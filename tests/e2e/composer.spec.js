// The editor (apps/web/frontend/composer.js) on the development fixture: local drafts, toolbar and menus. Behavior, not
// layout, so one viewport is enough; the fixture's forums (`daily`, `showcase`) and thread 104 are fixed.
import { expect, test } from '@playwright/test';
import { instances } from './support/pages.js';

const ready = (page) => expect(page.locator('form[data-composer][data-composer-ready="true"]').first()).toBeAttached();
// Every local draft of this browser, as one string.
const drafts = (page) => page.evaluate(() => Object.keys(localStorage).filter((key) => key.includes(':draft:'))
  .map((key) => localStorage.getItem(key)).join('\n'));

for (const instance of instances().filter((instance) => instance.content === 'fixture')) {
  test.describe(`composer (${instance.language})`, () => {
    test.use({ storageState: instance.adminState });
    test.beforeEach(({}, testInfo) => {
      test.skip(testInfo.project.name !== 'desktop-light', 'behavior, not layout: one viewport is enough');
    });

    test.describe('drafts', () => {
      test('an untouched new-thread page saves no draft over the next forum', async ({ page }) => {
        const forum = page.locator('select[name="fid"] option:checked');
        await page.goto(`${instance.origin}/new/daily`);
        await ready(page);
        await expect(forum).toHaveText('Daily');
        await page.goto(`${instance.origin}/new/showcase`);
        await ready(page);
        await expect(forum).toHaveText('Showcase');
        expect(await drafts(page)).toBe('');
      });

      test('a forum the page chose outranks the draft, which still fills an unchosen one', async ({ page }) => {
        const forum = page.locator('select[name="fid"] option:checked');
        const body = page.locator('textarea[name="body"]');
        await page.goto(`${instance.origin}/new/daily`);
        await ready(page);
        await body.fill('A draft started in Daily');
        await page.goto(`${instance.origin}/new/showcase`);
        await ready(page);
        await expect(body).toHaveValue('A draft started in Daily');
        await expect(forum).toHaveText('Showcase');
        await page.goto(`${instance.origin}/new`);
        await ready(page);
        await expect(body).toHaveValue('A draft started in Daily');
        await expect(forum).toHaveText('Daily');
      });

      test('a quoted reply gets its draft back after a refresh', async ({ page }) => {
        await page.goto(`${instance.origin}/thread-104`);
        const quote = await page.locator('a[href*="quote="]').first().getAttribute('href');
        await page.goto(new URL(quote, instance.origin).href);
        await ready(page);
        const body = page.locator('#reply-editor textarea[name="body"]');
        const prefilled = await body.inputValue();
        expect(prefilled).toContain('(post:');
        await body.fill(`${prefilled}My answer to the quote`);
        await expect.poll(() => drafts(page)).toContain('My answer to the quote');
        await page.reload();
        await ready(page);
        await expect(body).toHaveValue(`${prefilled}My answer to the quote`);
        // Leaving the restored, untouched page keeps the draft.
        await page.goto(`${instance.origin}/`);
        expect(await drafts(page)).toContain('My answer to the quote');
      });

      test('closing an untouched tab keeps the draft another tab saved', async ({ page, context }) => {
        const thread = `${instance.origin}/thread-104`;
        const body = (tab) => tab.locator('#reply-editor textarea[name="body"]');
        await page.goto(thread);
        await ready(page);
        const second = await context.newPage();
        await second.goto(thread);
        await ready(second);
        await body(page).fill('Typed in the first tab');
        await expect.poll(() => drafts(page)).toContain('Typed in the first tab');
        await second.close({ runBeforeUnload: true });
        const third = await context.newPage();
        await third.goto(thread);
        await ready(third);
        await expect(body(third)).toHaveValue('Typed in the first tab');
      });
    });
  });
}
