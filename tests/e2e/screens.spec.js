// Full-page screenshots of every checked page for the before/after comparison page. Files go to
// .local/visual/<label>/<language>/<project>/<page>.png, where the label comes from GONOF_VISUAL_LABEL
// (default `latest`); nothing is compared here, the comparison is judged by eye.
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { expect, test } from '@playwright/test';
import { root } from './support/instance.js';
import { instances, pages } from './support/pages.js';

const label = process.env.GONOF_VISUAL_LABEL || 'latest';

for (const instance of instances()) {
  for (const who of ['anonymous', 'admin']) {
    test.describe(`screens ${instance.language} ${who}`, () => {
      if (who === 'admin') test.use({ storageState: instance.adminState });
      for (const entry of pages.filter((entry) => entry.who === who)) {
        test(`${instance.language} ${entry.name}`, async ({ page }, info) => {
          const response = await page.goto(`${instance.origin}${entry.path}`, { waitUntil: 'networkidle' });
          expect(response.status()).toBe(entry.status || 200);
          const dir = join(root, '.local/visual', label, instance.language, info.project.name);
          mkdirSync(dir, { recursive: true });
          await page.screenshot({ path: join(dir, `${entry.name}.png`), fullPage: true, animations: 'disabled' });
        });
      }
    });
  }
}
