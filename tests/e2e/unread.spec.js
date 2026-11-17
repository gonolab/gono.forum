// Unread threads (docs/skins.md, "Unread"): the reading state site.js keeps in localStorage and the marks it puts on
// topic rows and the board directory. Behavior, not layout, so one viewport is enough; the cases post replies and
// change the home page, so they run one after another on an English instance of their own (development fixture),
// never on the instances the other checks share.
import { expect, request, test } from '@playwright/test';
import { launch } from './support/instance.js';

test.describe.configure({ mode: 'serial' });

/** The fixture's long thread: 78 replies; floors 2 to 55 are top-level and 56 to 79 nested under floor 2. */
const LONG = 103;
/** Short fixture threads with six replies. */
const SHORT = 104;
const OTHER = 105;

let instance;
let adminState;
let currentBrowser;

const key = (uid) => `gonof:${encodeURIComponent('/')}:read:${uid}`;
const stored = (page, uid) => page.evaluate((name) => JSON.parse(localStorage.getItem(name) || 'null'), key(uid));
const readerOf = (page) => page.locator('meta[name="gonof-reader"]').getAttribute('content');
const row = (page, tid) => page.locator(`ul[data-scroll-list] > li[data-thread-id="${tid}"]`);
const pageData = (page) => page.evaluate(() => JSON.parse(document.getElementById('gonof-page').textContent));
// Pages show times to the second, so a post counts as after the starting point only from the next whole second on.
const pastTheSecond = (since) => new Promise((done) => setTimeout(done, Math.max(0, Math.ceil(since / 1000) * 1000 + 50 - Date.now())));

async function signIn(context) {
  const page = await context.newPage();
  await page.goto(`${instance.origin}/login`);
  await page.fill('input[name="username"]', instance.admin.username);
  await page.fill('input[name="password"]', instance.admin.password);
  await Promise.all([page.waitForURL((url) => !url.pathname.startsWith('/login')), page.press('input[name="password"]', 'Enter')]);
  return page;
}

/** Posts a reply as the administrator through the thread page's reply box, which HTMX swaps in place. */
async function reply(tid, text) {
  const context = await currentBrowser.newContext({ storageState: adminState });
  const page = await context.newPage();
  await page.goto(`${instance.origin}/thread-${tid}`);
  await page.fill('#reply-editor textarea[name="body"]', text);
  await Promise.all([
    page.waitForResponse((answer) => answer.url().endsWith(`/api/thread-${tid}/comments`) && answer.request().method() === 'POST'),
    page.click('#reply-editor button[type="submit"]:not([formaction])'),
  ]);
  // The swapped replies section shows the new reply; the reply's own floor is the thread's highest.
  await expect(page.locator('#topic-replies .floor').filter({ hasText: text })).toHaveCount(1);
  const through = Number(await page.locator('#topic-replies').getAttribute('data-read-through'));
  const uid = await readerOf(page);
  // site.js recorded the floor of the swapped section: the author has read their own reply.
  await expect.poll(async () => (await stored(page, uid))?.threads?.[String(tid)]?.[0]).toBe(through);
  await context.close();
  return through;
}

test.beforeAll(async ({ browser }, testInfo) => {
  test.skip(testInfo.project.name !== 'desktop-light', 'behavior, not layout: one viewport is enough');
  testInfo.setTimeout(180000);
  currentBrowser = browser;
  instance = await launch({ language: 'en', label: 'unread' });
  const context = await request.newContext({ baseURL: instance.origin });
  const login = await (await context.get('/login')).text();
  const csrf = login.match(/name="_csrf" value="([^"]+)"/)[1];
  const answer = await context.post('/login', {
    form: { _csrf: csrf, username: instance.admin.username, password: instance.admin.password },
    maxRedirects: 0,
  });
  expect([302, 303]).toContain(answer.status());
  adminState = await context.storageState();
  await context.dispose();
});

test.afterAll(async () => {
  await instance?.stop();
});

test('the first visit writes the starting point and marks nothing', async ({ page }) => {
  const before = Date.now();
  await page.goto(`${instance.origin}/`);
  expect(await readerOf(page)).toBe('0');
  const state = await stored(page, '0');
  expect(state.since).toBeGreaterThanOrEqual(before);
  expect(state.threads).toEqual({});
  expect(await page.locator('ul[data-scroll-list] > li[data-thread-id][data-maxposition]').count()).toBeGreaterThan(5);
  await expect(page.locator('.is-unread, .row-unread')).toHaveCount(0);
});

test('opening page N records the floor that page counts as read up to', async ({ page }) => {
  await page.goto(`${instance.origin}/`);
  // A boosted navigation from the list: the window survives, the head and its page data block are the thread's.
  await page.evaluate(() => { window.unreadMarker = true; });
  await row(page, LONG).locator('.row-title').click();
  await page.waitForURL(`${instance.origin}/thread-${LONG}`);
  expect(await page.evaluate(() => window.unreadMarker)).toBe(true);
  // Nested page 1 shows nested replies up to floor 65 or so, but only floors up to 51 are all on page 1.
  expect((await pageData(page)).thread.read_through).toBe(51);
  await expect.poll(async () => (await stored(page, '0')).threads[String(LONG)]?.[0]).toBe(51);
  // The flat view's page 1 ends at the same floor; the record keeps its highest value.
  await page.goto(`${instance.origin}/thread-${LONG}?view=flat`);
  expect((await stored(page, '0')).threads[String(LONG)][0]).toBe(51);
  // Paging to the last page (boosted) counts the whole thread.
  await page.locator('.replies-pane__pager a.pager__item', { hasText: '2' }).click();
  await page.waitForURL(/thread-103-2/);
  await expect.poll(async () => (await stored(page, '0')).threads[String(LONG)][0]).toBe(79);
  await page.goto(`${instance.origin}/`);
  await expect(row(page, LONG)).not.toHaveClass(/is-unread/);
});

test('new replies make a thread unread, with their number once the thread was opened', async ({ page, context }) => {
  await page.goto(`${instance.origin}/`);
  await page.goto(`${instance.origin}/thread-${SHORT}`);
  const seen = (await stored(page, '0')).threads[String(SHORT)][0];
  expect(seen).toBe(Number(await page.locator('#topic-replies').getAttribute('data-read-through')));
  await page.goto(`${instance.origin}/`);
  await expect(page.locator('.is-unread')).toHaveCount(0);

  await pastTheSecond((await stored(page, '0')).since);
  const highest = await reply(SHORT, 'A new reply for the unread check.');
  expect(highest).toBe(seen + 1);
  await reply(OTHER, 'A reply to a thread this visitor never opened.');

  await page.goto(`${instance.origin}/`);
  const opened = row(page, SHORT);
  await expect(opened).toHaveClass(/is-unread/);
  await expect(opened).toHaveAttribute('data-unread', '1');
  await expect(opened.locator('.row-headline > .row-unread')).toHaveText('1 new reply');
  // The default look: a dot before the title; the words are for screen readers.
  const dot = await opened.locator('.row-title').evaluate((title) => getComputedStyle(title, '::before').content);
  expect(dot).toBe('""');
  expect(await opened.locator('.row-unread').evaluate((words) => getComputedStyle(words).position)).toBe('absolute');
  // Never opened: unread because its last activity is after the starting point, without a number.
  const never = row(page, OTHER);
  await expect(never).toHaveClass(/is-unread/);
  await expect(never).not.toHaveAttribute('data-unread', /.*/);
  await expect(never.locator('.row-unread')).toHaveText('Unread');
  await expect(page.locator('ul[data-scroll-list] > .is-unread')).toHaveCount(2);

  // Reading it in another tab updates this one through the storage event.
  const other = await context.newPage();
  await other.goto(`${instance.origin}/thread-${SHORT}`);
  await expect(opened).not.toHaveClass(/is-unread/);
  await expect(opened).not.toHaveAttribute('data-unread', /.*/);
  await expect(opened.locator('.row-unread')).toHaveCount(0);
  await expect(never).toHaveClass(/is-unread/);
});

test('at most 1,000 threads are kept, the least recently read dropped first', async ({ page }) => {
  await page.goto(`${instance.origin}/`);
  await page.evaluate((name) => {
    const threads = {};
    for (let index = 1; index <= 1000; index++) threads[String(200000 + index)] = [3, index];
    localStorage.setItem(name, JSON.stringify({ since: Date.now() - 60000, threads }));
  }, key('0'));
  await page.goto(`${instance.origin}/thread-${SHORT}`);
  const state = await stored(page, '0');
  const ids = Object.keys(state.threads);
  expect(ids).toHaveLength(1000);
  expect(ids).toContain(String(SHORT));
  expect(ids).not.toContain('200001');
  expect(ids).toContain('200002');
  expect(ids).toContain('201000');
});

test('visitors and members keep separate state; clearing forgets only the member\'s', async ({ browser }) => {
  const context = await browser.newContext();
  const guest = await context.newPage();
  await guest.goto(`${instance.origin}/`);
  await guest.goto(`${instance.origin}/thread-${OTHER}`);
  const guestState = await stored(guest, '0');
  expect(Object.keys(guestState.threads)).toEqual([String(OTHER)]);

  const member = await signIn(context);
  await member.goto(`${instance.origin}/`);
  const uid = await readerOf(member);
  expect(uid).toMatch(/^[1-9]\d*$/);
  // The member starts from their own starting point, with nothing of what the visitor read.
  const fresh = await stored(member, uid);
  expect(fresh.threads).toEqual({});
  expect(fresh.since).toBeGreaterThan(guestState.since);
  await member.goto(`${instance.origin}/thread-${LONG}?view=flat`);
  expect((await stored(member, uid)).threads).toEqual({ [String(LONG)]: [51, expect.any(Number)] });
  expect(await stored(member, '0')).toEqual(guestState);
  // Floors 52 to 79 are new to the member: the page 1 they read ends at 51.
  await member.goto(`${instance.origin}/`);
  await expect(row(member, LONG)).toHaveAttribute('data-unread', '28');

  // Signing out keeps both.
  await member.locator('.account-nav > summary').click();
  await Promise.all([
    member.waitForResponse((answer) => answer.url().endsWith('/logout') && answer.request().method() === 'POST'),
    member.locator('form.account-menu__logout button').click(),
  ]);
  await member.goto(`${instance.origin}/`);
  expect(await readerOf(member)).toBe('0');
  expect(await stored(member, '0')).toEqual(guestState);
  expect((await stored(member, uid)).threads[String(LONG)][0]).toBe(51);

  // Clearing in the reading preferences forgets this member's state only; the next list starts over, so the thread
  // read halfway is no longer unread.
  const again = await signIn(context);
  await again.goto(`${instance.origin}/my/reading`);
  const panel = again.locator('[data-device-settings]');
  await expect(panel).toBeVisible();
  const cleared = Date.now();
  await panel.getByRole('button', { name: 'Clear reading progress on this device' }).click();
  await expect(panel.locator('[data-read-status]')).toHaveText('Reading progress on this device cleared');
  expect(await stored(again, uid)).toBeNull();
  expect(await stored(again, '0')).toEqual(guestState);
  await again.goto(`${instance.origin}/`);
  const restarted = await stored(again, uid);
  expect(restarted.since).toBeGreaterThanOrEqual(cleared);
  expect(restarted.threads).toEqual({});
  await expect(row(again, LONG)).not.toHaveClass(/is-unread/);
  await context.close();
});

test('without JavaScript the pages carry the data and show no marks or device settings', async ({ browser }) => {
  const context = await browser.newContext({ javaScriptEnabled: false, storageState: adminState });
  const page = await context.newPage();
  const home = await page.goto(`${instance.origin}/`);
  expect(home.status()).toBe(200);
  expect(await page.locator('ul[data-scroll-list] > li[data-thread-id][data-maxposition]').count()).toBeGreaterThan(5);
  await expect(page.locator('.is-unread, .row-unread')).toHaveCount(0);
  await expect(row(page, LONG).locator('.row-title')).toBeVisible();
  const thread = await page.goto(`${instance.origin}/thread-${LONG}`);
  expect(thread.status()).toBe(200);
  await expect(page.locator('#topic-replies')).toHaveAttribute('data-read-through', '51');
  await page.goto(`${instance.origin}/my/reading`);
  await expect(page.locator('[data-device-settings]')).toBeHidden();
  await expect(page.locator('form[data-reading-form]')).toBeVisible();
  await context.close();
});

test('the board directory marks a board whose latest thread is unread', async ({ browser }) => {
  // Open the home page with the board directory: the layout's home page choice, published (docs/skins.md, "Editing a
  // layout").
  const admin = await request.newContext({ baseURL: instance.origin, storageState: adminState });
  const layout = await (await admin.get('/manage/layout')).text();
  const csrf = layout.match(/name="_csrf" value="([^"]+)"/)[1];
  const headers = { origin: instance.origin };
  for (const [path, form] of [['/manage/layout/home', { _csrf: csrf, domain: '0', home: 'boards' }], ['/manage/layout/publish', { _csrf: csrf, domain: '0' }]]) {
    const answer = await admin.post(path, { headers, form, maxRedirects: 0 });
    expect(answer.status(), path).toBe(303);
  }
  await admin.dispose();

  const context = await browser.newContext();
  const page = await context.newPage();
  await page.goto(`${instance.origin}/`);
  await expect(page.locator('[data-board-directory]')).toBeVisible();
  await expect(page.locator('.board-dir__item.is-unread')).toHaveCount(0);
  await page.goto(`${instance.origin}/thread-${SHORT}`);
  const seen = (await stored(page, '0')).threads[String(SHORT)][0];

  // A reply makes SHORT the latest thread of its board, one floor past what this visitor read.
  await pastTheSecond((await stored(page, '0')).since);
  await reply(SHORT, 'Another reply, for the board directory.');
  await page.goto(`${instance.origin}/`);
  const board = (await pageData(page)).directory.find((entry) => entry.last_post?.tid === SHORT);
  expect(board.last_post.max_position).toBe(seen + 1);
  const item = page.locator(`.board-dir__item[data-board="${board.fid}"]`);
  await expect(item).toHaveClass(/is-unread/);
  await expect(item).toHaveAttribute('data-unread', '1');
  await expect(item.locator('.board-dir__last-text > .row-unread')).toHaveText('1 new reply');
  expect(await item.locator('.board-dir__last-title').evaluate((title) => getComputedStyle(title, '::before').content)).toBe('""');
  await expect(page.locator('.board-dir__item.is-unread')).toHaveCount(1);
  // Reading it clears the mark.
  await page.goto(`${instance.origin}/thread-${SHORT}`);
  await page.goto(`${instance.origin}/`);
  await expect(item).not.toHaveClass(/is-unread/);
  await context.close();
});
