// The update page's wait during a one-click upgrade (waitForUpgrade in apps/web/static/js/site.js): it reloads only
// once the forum answers, never onto the reverse proxy's 502 page that appears while systemd restarts the service. A
// source build cannot start an upgrade, so the page and the forum's answers are simulated by routing requests to
// /manage/updates; site.js is the real file served by the English instance.
import { expect, test } from '@playwright/test';
import { instances } from './support/pages.js';

const WAITING = '<!doctype html><html><head><script src="/static/js/site.js"></script></head>'
  + '<body><section data-upgrade-wait><p id="waiting">Downloading</p></section></body></html>';
const DONE = '<!doctype html><html><body><p id="done">Upgraded</p></body></html>';
const GATEWAY = '<!doctype html><html><body>502 Bad Gateway</body></html>';

test('the update page waits through the restart gap and reloads once the forum answers', async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== 'desktop-light', 'behavior, not layout: one viewport is enough');
  const instance = instances().find((entry) => entry.language === 'en');
  const address = `${instance.origin}/manage/updates?notice=started`;
  const errors = [];
  page.on('pageerror', (error) => errors.push(error.message));
  const answers = [];
  await page.route((url) => url.pathname === '/manage/updates', (route) => {
    const kind = route.request().resourceType();
    if (kind === 'document') {
      const first = !answers.some((answer) => answer.kind === 'document');
      answers.push({ kind, status: 200 });
      return route.fulfill({ status: 200, contentType: 'text/html', body: first ? WAITING : DONE });
    }
    // The waiting script's requests: twice the gap while systemd restarts the service, then the new version.
    const status = answers.filter((answer) => answer.kind === 'fetch').length < 2 ? 502 : 200;
    answers.push({ kind, status });
    return route.fulfill({ status, contentType: 'text/html', body: status === 200 ? DONE : GATEWAY });
  });

  await page.goto(address);
  await expect(page.locator('#done')).toBeVisible({ timeout: 20000 });
  expect(answers).toEqual([
    { kind: 'document', status: 200 },
    { kind: 'fetch', status: 502 },
    { kind: 'fetch', status: 502 },
    { kind: 'fetch', status: 200 },
    { kind: 'document', status: 200 },
  ]);
  expect(page.url()).toBe(address);
  expect(errors).toEqual([]);
});
