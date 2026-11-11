import { build } from 'esbuild';

await build({
  entryPoints: ['apps/web/frontend/composer.js'],
  outfile: 'apps/web/static/js/composer.js',
  bundle: true,
  format: 'iife',
  platform: 'browser',
  target: ['es2022'],
  minify: true,
  legalComments: 'eof',
  charset: 'utf8',
  logLevel: 'info',
});
