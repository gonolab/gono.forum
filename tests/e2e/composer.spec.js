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

    test.describe('toolbar and menus', () => {
      test('the code button fences a selection with a longer fence than any inside it', async ({ page }) => {
        await page.goto(`${instance.origin}/new`);
        await ready(page);
        const body = page.locator('textarea[name="body"]');
        const source = '```\n## Literal heading\n```';
        await body.fill(source);
        await body.evaluate((field) => field.setSelectionRange(0, field.value.length));
        await page.locator('[data-editor-action="code"]').click();
        await expect(body).toHaveValue(`\n\`\`\`\`\n${source}\n\`\`\`\`\n`);
        await page.locator('[data-editor-action="preview"]').click();
        const preview = page.locator('[data-editor-preview]');
        await expect(preview.locator('pre')).toContainText('## Literal heading');
        await expect(preview.locator('h2')).toHaveCount(0);
      });

      test('Escape that closes a menu or full screen leaves the docked reply box open', async ({ page }) => {
        await page.goto(`${instance.origin}/thread-104`);
        await ready(page);
        // The default skin does not dock the reply box: turn the behavior module on for this page only.
        await page.evaluate(() => { document.documentElement.dataset.behaviors = 'docked-composer'; });
        await page.addScriptTag({ url: `${instance.origin}/static/js/behaviors/docked-composer.js` });
        const dock = page.locator('html');
        const toggle = page.locator('div[data-composer-dock] > button');
        await toggle.click();
        await expect(dock).toHaveAttribute('data-composer-dock', 'open');
        await expect(page.locator('#reply-editor textarea[name="body"]')).toBeFocused();
        const picker = page.locator('#reply-editor [data-editor-picker]');
        await page.locator('#reply-editor [data-editor-action="emoji"]').click();
        await expect(picker).toBeVisible();
        await page.keyboard.press('Escape');
        await expect(picker).toBeHidden();
        await expect(dock).toHaveAttribute('data-composer-dock', 'open');
        await page.locator('#reply-editor [data-editor-action="fullscreen"]').click();
        await expect(dock).toHaveClass(/has-write-fullscreen/);
        await page.keyboard.press('Escape');
        await expect(dock).not.toHaveClass(/has-write-fullscreen/);
        await expect(dock).toHaveAttribute('data-composer-dock', 'open');
        // With nothing of the editor's left open, Escape closes the box.
        await page.keyboard.press('Escape');
        await expect(dock).toHaveAttribute('data-composer-dock', 'closed');
        await expect(toggle).toBeFocused();
      });

      test('Enter right after changing the mention search inserts a member found for the new term', async ({ page }) => {
        const members = { camellia: '41', sprout: '42' };
        await page.route('**/api/mentions?*', async (route) => {
          const term = new URL(route.request().url()).searchParams.get('q').trim();
          const found = Object.entries(members).filter(([name]) => name.startsWith(term));
          await route.fulfill({
            contentType: 'text/html',
            body: `<ul>${found.map(([name, uid]) => `<li data-uid="${uid}" data-username="${name}">${name}</li>`).join('')}</ul>`,
          });
        });
        await page.goto(`${instance.origin}/new`);
        await ready(page);
        const body = page.locator('textarea[name="body"]');
        await body.click();
        await page.locator('[data-editor-action="mention"]').click();
        const search = page.locator('[data-mention-query]');
        await search.fill('camellia');
        await expect(page.locator('.mention-menu button')).toHaveText(['@camellia']);
        await search.fill('sprout');
        await search.press('Enter');
        await expect(body).toHaveValue('[@sprout](member:42) ');
      });
    });
  });
}
