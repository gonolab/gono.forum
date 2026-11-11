import { build } from 'esbuild';
import { readFile, readdir } from 'node:fs/promises';
import { resolve } from 'node:path';

const source = resolve('apps/web/frontend/styles');
const themeDir = resolve(source, 'themes');
const themeFiles = (await readdir(themeDir)).filter(name => name.endsWith('.json')).sort();
const defaultPalette = 'default';
if (!themeFiles.includes(`${defaultPalette}.json`)) throw new Error(`The default ${defaultPalette} palette is required`);
themeFiles.sort((a, b) => (a === `${defaultPalette}.json` ? -1 : b === `${defaultPalette}.json` ? 1 : a.localeCompare(b)));
const block = (selector, tokens) => `${selector} {\n${Object.entries(tokens).map(([name, value]) => `  ${name}: ${value};`).join('\n')}\n}\n`;
let themes = '/* Generated from frontend/styles/themes; do not edit build output. */\n';
let contract;
for (const file of themeFiles) {
  const name = file.slice(0, -5);
  if (!/^[a-z][a-z0-9.-]*$/.test(name)) throw new Error(`Invalid palette name: ${name}`);
  const { shared, light, dark } = JSON.parse(await readFile(resolve(themeDir, file), 'utf8'));
  if (!shared || !light || !dark) throw new Error(`${file}: shared, light and dark are required`);
  const modes = { light: { ...shared, ...light }, dark: { ...shared, ...light, ...dark } };
  const keys = Object.keys(modes.light).sort().join('\n');
  if (Object.keys(modes.dark).sort().join('\n') !== keys) throw new Error(`${file}: light/dark token contracts differ`);
  if (contract && contract !== keys) throw new Error(`${file}: palette token contract differs from the default palette`);
  contract = keys;
  for (const [mode, tokens] of Object.entries(modes)) {
    for (const [token, value] of Object.entries(tokens)) {
      if (!/^--[a-z][a-z0-9-]*$/.test(token) || typeof value !== 'string' || /[{};]|@import/i.test(value) || (value.includes("url(") && !/^url\("data:image\/svg\+xml,[^"<>]+"\)$/.test(value))) {
        throw new Error(`${file}: invalid token ${token}`);
      }
      const visit = (key, chain = []) => {
        if (chain.includes(key)) throw new Error(`${file}: cyclic token ${[...chain, key].join(' -> ')}`);
        for (const match of (tokens[key] || '').matchAll(/var\((--[\w-]+)/g)) {
          if (!(match[1] in tokens)) throw new Error(`${file}: undefined theme token ${match[1]}`);
          visit(match[1], [...chain, key]);
        }
      };
      visit(token);
    }
    // The mode is independent of the palette. Omitting data-theme follows the OS,
    // including with JavaScript disabled. Dark values have only one authored source.
    const roots = [`:root[data-palette="${name}"]`, ...(name === defaultPalette ? [':root:not([data-palette])'] : [])];
    if (mode === 'light') themes += block(roots.join(', '), { 'color-scheme': 'light', ...tokens });
    else {
      const values = { 'color-scheme': 'dark', ...tokens };
      themes += block(roots.map(root => `${root}[data-theme="dark"]`).join(', '), values);
      themes += `@media (prefers-color-scheme: dark) {\n${block(roots.map(root => `${root}:not([data-theme="light"]):not([data-theme="dark"])`).join(', '), values)}}\n`;
    }
  }
}
await build({
  stdin: { contents: '@import "forum:themes";\n@import "./index.css";', resolveDir: source, loader: 'css' },
  plugins: [{ name: 'forum-themes', setup(api) {
    api.onResolve({ filter: /^forum:themes$/ }, () => ({ path: 'themes', namespace: 'forum' }));
    api.onLoad({ filter: /.*/, namespace: 'forum' }, () => ({ contents: themes, loader: 'css' }));
  } }],
  outfile: 'apps/web/static/css/forum.css',
  bundle: true, minify: false, charset: 'utf8', logLevel: 'info',
});
await build({
  entryPoints: [resolve(source, 'debug.css')],
  outfile: 'apps/web/static/css/debug.css',
  bundle: true, minify: false, charset: 'utf8', logLevel: 'info',
});
