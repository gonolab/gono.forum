// Reading-page scripts (static/js/site.js, navigation.js and the timeline and progress behavior modules): the address
// that follows the floor being read, the frame attributes a boosted navigation carries, the recently viewed blocks, the
// phone drawer, the list position on going back, the timeline and progress blocks, and the unread starting point.
// Behavior, not layout, so the cases run once, in the desktop-light project (phone cases in a phone-sized context of
// their own), one after another on an English instance of their own (development fixture): they post, change the site
// language and choose a skin made for them, so they never share the instances the other checks use.
//
// The skin is Atrium (side navigation, board directory home, the timeline and progress modules) with two recently
// viewed blocks on the home page and, on thread pages, a third outline block: a timeline of thread 103 whatever the
// thread. The autohide header and the docked composer stay off, so the header height and the reply box stay put.
import { execFileSync } from 'node:child_process';
import { cpSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { expect, request, test } from '@playwright/test';
import { launch, root } from './support/instance.js';

test.describe.configure({ mode: 'serial' });

/** The fixture's long thread: 78 replies; floors 2 to 55 are top-level and 56 to 79 nested under floor 2. */
const LONG = 103;
/** Short fixture threads with six replies (seven floors). */
const SHORT = 104;
const OTHER = 105;

const phone = { viewport: { width: 390, height: 844 }, deviceScaleFactor: 2, isMobile: true, hasTouch: true };

let instance;
let adminState;
let currentBrowser;

const readKey = (uid) => `gonof:${encodeURIComponent('/')}:read:${uid}`;
const stored = (page, uid) => page.evaluate((name) => JSON.parse(localStorage.getItem(name) || 'null'), readKey(uid));
const row = (page, tid) => page.locator(`ul[data-scroll-list] > li[data-thread-id="${tid}"]`);
// Pages show times to the second, so a post counts as after the starting point only from the next whole second on. The
// starting point is the server's time, which pages write in whole seconds, so it can fall exactly on one.
const pastTheSecond = (since) => new Promise((done) => setTimeout(done, Math.max(0, Math.floor(since / 1000) * 1000 + 1050 - Date.now())));

/** The value of a hidden input in the page, with Askama's HTML escaping undone. */
function hidden(page, name) {
  const value = page.match(new RegExp(`name="${name}" value="([^"]*)"`))?.[1];
  if (value === undefined) return undefined;
  const named = { quot: '"', apos: "'", lt: '<', gt: '>', amp: '&' };
  return value.replace(/&(#x[0-9a-f]+|#[0-9]+|[a-z]+);/gi, (entity, code) => {
    if (code[0] !== '#') return named[code] ?? entity;
    return String.fromCodePoint(code[1] === 'x' || code[1] === 'X' ? parseInt(code.slice(2), 16) : parseInt(code.slice(1), 10));
  });
}

/** Packs Atrium with the layout above (gono-forum-cli skin pack), imports it and makes it the site's skin. */
async function useReadingSkin(context) {
  const dir = mkdtempSync(join(tmpdir(), 'gonof-e2e-skin-'));
  try {
    const skin = join(dir, 'reading-e2e');
    cpSync(join(root, 'apps/web/skins/atrium'), skin, { recursive: true });
    const manifest = join(skin, 'skin.toml');
    const outline = '{ id = "outline-pages", source = "thread.outline", view = "progress", params = { tid = "current" }, title = { key = "block-title-navigation" } },';
    let text = readFileSync(manifest, 'utf8');
    for (const [from, to] of [
      ['id = "atrium"', 'id = "reading-e2e"'],
      ['name = { en = "Atrium" }', 'name = { en = "Reading checks" }'],
      ['enabled = ["timeline", "progress", "autohide-header", "docked-composer"]', 'enabled = ["timeline", "progress"]'],
      ['"home.aside" = []', '"home.aside" = [\n  { id = "recent-one", source = "recent-local" },\n  { id = "recent-two", source = "recent-local" },\n]'],
      [outline, `${outline}\n  { id = "outline-other", source = "thread.outline", view = "timeline", params = { tid = ${LONG} }, title = { key = "block-title-outline" } },`],
    ]) {
      if (!text.includes(from)) throw new Error(`atrium's skin.toml no longer has ${from}`);
      text = text.replace(from, to);
    }
    writeFileSync(manifest, text);
    const pack = join(dir, 'pack.json');
    const cli = join(root, process.env.CARGO_TARGET_DIR || 'target', 'debug', 'gono-forum-cli');
    execFileSync(cli, ['skin', 'pack', skin, `--output=${pack}`], { cwd: root, stdio: 'pipe' });
    const headers = { origin: instance.origin };
    let page = await (await context.get('/manage/skins')).text();
    const csrf = hidden(page, '_csrf');
    let answer = await context.post('/manage/skins/import', {
      headers,
      multipart: { _csrf: csrf, pack: { name: 'reading-e2e.skin.json', mimeType: 'application/json', buffer: readFileSync(pack) } },
      maxRedirects: 0,
    });
    if (answer.status() === 200) {
      page = await answer.text();
      answer = await context.post('/manage/skins/import/confirm', {
        headers,
        form: { _csrf: csrf, replaces: hidden(page, 'replaces'), digest: hidden(page, 'digest'), pack: hidden(page, 'pack') },
        maxRedirects: 0,
      });
    }
    expect(answer.status()).toBe(303);
    answer = await context.post('/manage/skins/select', { headers, form: { _csrf: csrf, choice: 'reading-e2e:' }, maxRedirects: 0 });
    expect(answer.status()).toBe(303);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

/** Sets the site language at /manage/languages as the administrator. */
async function siteLanguage(language) {
  const context = await request.newContext({ baseURL: instance.origin, storageState: adminState });
  try {
    const csrf = hidden(await (await context.get('/manage/languages')).text(), '_csrf');
    const answer = await context.post('/manage/languages', {
      headers: { origin: instance.origin },
      form: { _csrf: csrf, language },
      maxRedirects: 0,
    });
    expect(answer.status()).toBe(303);
  } finally {
    await context.dispose();
  }
}

/** Posts a reply on `page` (signed in as the administrator) through the thread page's reply box, which HTMX swaps. */
async function replyOn(page, tid, text) {
  await page.fill('#reply-editor textarea[name="body"]', text);
  await Promise.all([
    page.waitForResponse((answer) => answer.url().endsWith(`/api/thread-${tid}/comments`) && answer.request().method() === 'POST'),
    page.click('#reply-editor button[type="submit"]:not([formaction])'),
  ]);
  await expect(page.locator('#topic-replies .floor').filter({ hasText: text })).toHaveCount(1);
}

/** Posts a reply as the administrator in a context of its own. */
async function reply(tid, text) {
  const context = await currentBrowser.newContext({ storageState: adminState });
  const page = await context.newPage();
  await page.goto(`${instance.origin}/thread-${tid}`);
  await replyOn(page, tid, text);
  await context.close();
}

/** Starts a thread in the Daily board as the administrator; returns its id. */
async function startThread(title) {
  const context = await currentBrowser.newContext({ storageState: adminState });
  const page = await context.newPage();
  await page.goto(`${instance.origin}/new/daily`);
  await expect(page.locator('form[data-composer][data-composer-ready="true"]').first()).toBeAttached();
  await page.fill('input[name="title"]', title);
  await page.fill('textarea[name="body"]', `${title}: the opening post of a browser check.`);
  await Promise.all([
    page.waitForURL(/\/thread-\d+/),
    page.click('form[data-composer] button[type="submit"]:not([formaction])'),
  ]);
  const tid = Number(new URL(page.url()).pathname.match(/thread-(\d+)/)[1]);
  await context.close();
  return tid;
}

test.beforeAll(async ({ browser }, testInfo) => {
  test.skip(testInfo.project.name !== 'desktop-light', 'behavior, not layout: one viewport is enough');
  testInfo.setTimeout(180000);
  currentBrowser = browser;
  instance = await launch({ language: 'en', label: 'reading' });
  const context = await request.newContext({ baseURL: instance.origin });
  const login = await (await context.get('/login')).text();
  const answer = await context.post('/login', {
    form: { _csrf: hidden(login, '_csrf'), username: instance.admin.username, password: instance.admin.password },
    maxRedirects: 0,
  });
  expect([302, 303]).toContain(answer.status());
  adminState = await context.storageState();
  await useReadingSkin(context);
  await context.dispose();
});

test.afterAll(async () => {
  await instance?.stop();
});

test('the address follows the innermost floor of a nested reply', async ({ page }) => {
  await page.goto(`${instance.origin}/thread-${LONG}`);
  // The nested view: floor 56 is a nested reply inside floor 2's element.
  await expect(page.locator('#floor-2 #floor-56')).toHaveCount(1);
  await page.evaluate(() => {
    const floor = document.getElementById('floor-56');
    scrollTo({ top: scrollY + floor.getBoundingClientRect().top - window.gonofStickyTop() - 8, behavior: 'instant' });
  });
  await expect.poll(() => page.evaluate(() => location.hash)).toBe('#floor-56');
});

test('a boosted navigation takes the language and direction of the new page', async ({ page }) => {
  await page.goto(`${instance.origin}/`);
  await expect(page.locator('html')).toHaveAttribute('lang', 'en');
  // As if the page had been in a right-to-left language: the next page's <html> has no dir.
  await page.evaluate(() => { document.documentElement.dir = 'rtl'; window.readingMarker = true; });
  await siteLanguage('zh-CN');
  try {
    await page.locator('.board-dir__name a[href$="/daily"]').click();
    await page.waitForURL(/\/daily$/);
    // Still the same window: the navigation was boosted.
    expect(await page.evaluate(() => window.readingMarker)).toBe(true);
    await expect(page.locator('meta[property="og:locale"]')).toHaveAttribute('content', 'zh_CN');
    await expect(page.locator('html')).toHaveAttribute('lang', 'zh-CN');
    await expect(page.locator('html')).not.toHaveAttribute('dir', /.*/);
  } finally {
    await siteLanguage('en');
  }
});

test('every recently viewed block on a page lists the threads', async ({ page }) => {
  await page.goto(`${instance.origin}/thread-${SHORT}`);
  await page.goto(`${instance.origin}/`);
  for (const id of ['recent-one', 'recent-two']) {
    const block = page.locator(`[data-recent-threads][data-block="${id}"]`);
    await expect(block).toBeVisible();
    await expect(block.locator('[data-recent-list] > li a')).toHaveCount(1);
    await expect(block.locator('[data-recent-list] a')).toHaveAttribute('href', `/thread-${SHORT}`);
  }
  // Clearing in one block clears what both show.
  await page.locator('[data-block="recent-two"] [data-recent-clear]').click();
  for (const id of ['recent-one', 'recent-two']) {
    await expect(page.locator(`[data-block="${id}"] [data-recent-list] > li a`)).toHaveCount(0);
  }
});

test('the open phone drawer leaves the page it covers inert', async () => {
  const context = await currentBrowser.newContext(phone);
  const page = await context.newPage();
  try {
    await page.goto(`${instance.origin}/`);
    const inert = (selector) => page.locator(selector).evaluate((element) => element.closest('[inert]') !== null);
    expect(await page.locator('[inert]').count()).toBe(0);
    await page.locator('.drawer-toggle').click();
    await expect(page.locator('.drawer__close')).toBeFocused();
    for (const selector of ['#main', '.skip-link', '.masthead__brand']) {
      expect(await inert(selector), selector).toBe(true);
    }
    expect(await inert('.drawer')).toBe(false);
    expect(await inert('.drawer-toggle')).toBe(false);
    // A link the overlay covers cannot take focus.
    const covered = page.locator('#main a[href]').first();
    await covered.evaluate((link) => link.focus());
    await expect(covered).not.toBeFocused();
    // Closing gives the page back, and only what the drawer made inert.
    await page.keyboard.press('Escape');
    await expect(page.locator('.drawer-toggle')).toBeFocused();
    await expect(page.locator('[inert]')).toHaveCount(0);
    await covered.evaluate((link) => link.focus());
    await expect(covered).toBeFocused();
  } finally {
    await context.close();
  }
});

test('pressing the timeline thumb without dragging stays on the floor being read', async ({ page }) => {
  await page.goto(`${instance.origin}/thread-${LONG}`);
  const handle = page.locator('[data-block="outline"] .timeline__handle');
  await expect(handle).toHaveAttribute('aria-valuemax', '79');
  await expect(handle).toHaveAttribute('aria-valuenow', '1');
  expect(await page.evaluate(() => scrollY)).toBe(0);
  const box = await handle.boundingBox();
  await page.mouse.click(box.x + box.width / 2, box.y + box.height / 2);
  await page.waitForTimeout(300);
  await expect(handle).toHaveAttribute('aria-valuenow', '1');
  expect(await page.evaluate(() => scrollY)).toBe(0);
  // Dragging moves the thumb by as much as the pointer: the thumb's top to the track's end is its whole travel.
  const track = await page.locator('[data-block="outline"] .timeline__track').boundingBox();
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2 + (track.height - box.height) / 2, { steps: 4 });
  await expect(handle).toHaveAttribute('aria-valuenow', '40');
  await page.mouse.move(box.x + box.width / 2, track.y + track.height + 50, { steps: 4 });
  await expect(handle).toHaveAttribute('aria-valuenow', '79');
  await page.keyboard.press('Escape');
  await page.mouse.up();
});

test('a timeline of another thread opens that thread', async ({ page }) => {
  await page.goto(`${instance.origin}/thread-${SHORT}`);
  // This page has floors 1 to 7 too.
  await expect(page.locator('#floor-2')).toHaveCount(1);
  const handle = page.locator('[data-block="outline-other"] .timeline__handle');
  await expect(handle).toHaveAttribute('aria-valuemax', '79');
  await handle.focus();
  await handle.press('ArrowDown');
  await expect(handle).toHaveAttribute('aria-valuenow', '2');
  await Promise.all([page.waitForURL(`${instance.origin}/thread-${LONG}?view=flat#floor-2`), handle.press('Enter')]);
});

test('the timeline and progress blocks take in a reply posted in place', async () => {
  const context = await currentBrowser.newContext({ storageState: adminState });
  const page = await context.newPage();
  try {
    await page.goto(`${instance.origin}/thread-${OTHER}`);
    const handle = page.locator('[data-block="outline"] .timeline__handle');
    const meter = page.locator('[data-block="outline-pages"] progress');
    await expect(handle).toHaveAttribute('aria-valuemax', '7');
    expect(await meter.evaluate((element) => element.max)).toBe(7);
    await expect(page.locator('#topic-replies')).toHaveAttribute('data-last-position', '7');
    await replyOn(page, OTHER, 'An eighth floor for the timeline check.');
    await expect(page.locator('#topic-replies')).toHaveAttribute('data-last-position', '8');
    await expect(handle).toHaveAttribute('aria-valuemax', '8');
    await expect.poll(() => meter.evaluate((element) => element.max)).toBe(8);
    await handle.focus();
    await handle.press('End');
    await expect(handle).toHaveAttribute('aria-valuenow', '8');
    // Enter scrolls to the new floor on this page.
    await handle.press('Enter');
    await expect.poll(() => page.evaluate(() => location.hash)).toBe('#floor-8');
    expect(page.url()).toContain(`/thread-${OTHER}`);
    // The timeline of another thread keeps its own floors.
    await expect(page.locator('[data-block="outline-other"] .timeline__handle')).toHaveAttribute('aria-valuemax', '79');
  } finally {
    await context.close();
  }
});

test('an empty search writes the starting point, so the first new match is unread', async ({ page }) => {
  const word = `Quokka${Date.now()}`;
  await page.goto(`${instance.origin}/search?q=${word}`);
  await expect(page.locator('ul.rows--topics > li[data-thread-id]')).toHaveCount(0);
  const state = await stored(page, '0');
  expect(state?.since).toBeGreaterThan(0);
  await pastTheSecond(state.since);
  const tid = await startThread(`${word} arrives`);
  await page.goto(`${instance.origin}/search?q=${word}`);
  await expect(row(page, tid)).toHaveClass(/is-unread/);
});

test('a device clock an hour fast still marks a new reply unread', async () => {
  const context = await currentBrowser.newContext();
  const page = await context.newPage();
  try {
    const hour = 3600 * 1000;
    await page.clock.setSystemTime(Date.now() + hour);
    await page.goto(`${instance.origin}/`);
    const data = await page.evaluate(() => JSON.parse(document.getElementById('gonof-page').textContent));
    const server = Date.parse(data.now);
    expect(Math.abs(server - Date.now())).toBeLessThan(60 * 1000);
    const state = await stored(page, '0');
    // The starting point is the server's time, not this device's.
    expect(state.since).toBeLessThan(Date.now() + hour / 2);
    await pastTheSecond(Math.max(state.since, server));
    await reply(SHORT, 'A reply after a visit from a fast clock.');
    await page.goto(`${instance.origin}/`);
    await expect(row(page, SHORT)).toHaveClass(/is-unread/);
  } finally {
    await context.close();
  }
});
