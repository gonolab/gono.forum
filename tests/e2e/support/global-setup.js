// Starts the instances for the whole run: English with the development fixture (English text) and Simplified Chinese
// with the setup wizard's Chinese sample content, so the Chinese pages carry Chinese titles, names and posts. Signs the administrator in once per instance (sign-ins share one per-IP rate limit on
// loopback) and hands origins and saved sessions to the workers through the environment; the returned function is
// Playwright's global teardown.
import { readFileSync } from 'node:fs';
import { basename, join } from 'node:path';
import { request } from '@playwright/test';
import { launch } from './instance.js';

async function signIn(instance) {
  const context = await request.newContext({ baseURL: instance.origin });
  const page = await (await context.get('/login')).text();
  const csrf = page.match(/name="_csrf" value="([^"]+)"/)?.[1];
  if (!csrf) throw new Error(`${instance.language}: no CSRF token on the sign-in page`);
  const answer = await context.post('/login', {
    form: { _csrf: csrf, username: instance.admin.username, password: instance.admin.password },
    maxRedirects: 0,
  });
  if (answer.status() !== 303 && answer.status() !== 302) {
    throw new Error(`${instance.language}: sign-in answered ${answer.status()}`);
  }
  const path = join(instance.dir, 'admin-state.json');
  await context.storageState({ path });
  await context.dispose();
  return path;
}

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

/**
 * With GONOF_E2E_SKIN set to a skin pack (gono-forum-cli skin pack), imports it through /manage/skins, confirming
 * its warnings, and makes it the site's skin, so the checks and screenshots run with it (docs/skins.md).
 */
async function useSkin(instance, state, pack) {
  const context = await request.newContext({ baseURL: instance.origin, storageState: state });
  try {
    const headers = { origin: instance.origin };
    let page = await (await context.get('/manage/skins')).text();
    const csrf = hidden(page, '_csrf');
    let answer = await context.post('/manage/skins/import', {
      headers,
      multipart: { _csrf: csrf, pack: { name: basename(pack), mimeType: 'application/json', buffer: readFileSync(pack) } },
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
    if (answer.status() !== 303) {
      const reason = (await answer.text()).match(/data-skin-refused>([\s\S]*?)<\/div>/)?.[1] ?? '';
      throw new Error(`${instance.language}: importing ${pack} answered ${answer.status()} ${reason}`);
    }
    const id = JSON.parse(readFileSync(pack, 'utf8')).manifest.id;
    answer = await context.post('/manage/skins/select', { headers, form: { _csrf: csrf, choice: `${id}:` }, maxRedirects: 0 });
    if (answer.status() !== 303) throw new Error(`${instance.language}: choosing ${id} answered ${answer.status()}`);
  } finally {
    await context.dispose();
  }
}

export default async function globalSetup() {
  // Both start at once; when one fails, the one that did start is stopped too (a failed one has cleaned up itself).
  const started = await Promise.allSettled([launch({ language: 'en' }), launch({ language: 'zh-CN', content: 'sample' })]);
  const instances = started.filter((result) => result.status === 'fulfilled').map((result) => result.value);
  const failed = started.find((result) => result.status === 'rejected');
  if (failed) {
    await Promise.all(instances.map((instance) => instance.stop()));
    throw failed.reason;
  }
  try {
    const states = await Promise.all(instances.map(signIn));
    if (process.env.GONOF_E2E_SKIN) {
      await Promise.all(instances.map((instance, index) => useSkin(instance, states[index], process.env.GONOF_E2E_SKIN)));
    }
    process.env.GONOF_E2E_INSTANCES = JSON.stringify(
      instances.map(({ origin, language, content, admin }, index) => ({
        origin,
        language,
        content,
        admin,
        adminState: states[index],
      })),
    );
  } catch (error) {
    await Promise.all(instances.map((instance) => instance.stop()));
    throw error;
  }
  return async () => {
    await Promise.all(instances.map((instance) => instance.stop()));
  };
}
