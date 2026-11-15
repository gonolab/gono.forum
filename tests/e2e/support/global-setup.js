// Starts the instances for the whole run: English with the development fixture (English text) and Simplified Chinese
// with the setup wizard's Chinese sample content, so the Chinese pages carry Chinese titles, names and posts. Signs the administrator in once per instance (sign-ins share one per-IP rate limit on
// loopback) and hands origins and saved sessions to the workers through the environment; the returned function is
// Playwright's global teardown.
import { join } from 'node:path';
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
