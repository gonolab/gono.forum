// Throwaway local instances for the browser checks. Each instance gets its own temporary directory with a SQLite
// database, a private configuration with fresh random keys, Redis off and a free port; it is migrated, set to the
// requested site language and taken through the setup wizard's administrator stage. Its content is either the
// development fixture (`seed-demo`, before the wizard) or the wizard's own sample content in the site language,
// ticked on the wizard form. Nothing here talks to MySQL, Redis, the development site's .local or any remote host.
import { execFileSync, spawn } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { existsSync, mkdirSync, mkdtempSync, openSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import net from 'node:net';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { fileURLToPath } from 'node:url';

export const root = resolve(dirname(fileURLToPath(import.meta.url)), '../../..');
const binaries = join(root, process.env.CARGO_TARGET_DIR || 'target', 'debug');
const web = join(binaries, 'gono-forum-web');
const cli = join(binaries, 'gono-forum-cli');

export const admin = { username: 'admin', email: 'admin@example.test', password: 'e2e-Admin-Pass-1' };

function hex() {
  return randomBytes(32).toString('hex');
}

function freePort() {
  return new Promise((done, fail) => {
    const server = net.createServer();
    server.unref();
    server.on('error', fail);
    server.listen(0, '127.0.0.1', () => {
      const { port } = server.address();
      server.close(() => done(port));
    });
  });
}

// Only GONOF_CONFIG reaches the instance: variables such as GONOF_DATABASE_URL in the caller's shell would
// otherwise override the private configuration and point the instance at a real database.
function environment(config) {
  const env = { ...process.env };
  for (const name of Object.keys(env)) {
    if (name.startsWith('GONOF_')) delete env[name];
  }
  env.GONOF_CONFIG = config;
  env.RUST_LOG = env.RUST_LOG || 'warn';
  return env;
}

async function waitFor(check, what, timeout = 60000) {
  const deadline = Date.now() + timeout;
  let last;
  while (Date.now() < deadline) {
    try {
      if (await check()) return;
    } catch (error) {
      last = error;
    }
    await new Promise((done) => setTimeout(done, 200));
  }
  throw new Error(`timed out waiting for ${what}${last ? `: ${last.message}` : ''}`);
}

/**
 * Starts one instance. `language` is the site language (`en`, `zh-CN`, ...); `content` is `fixture` (the development
 * fixture) or `sample` (the setup wizard's sample content in `language`); `label` only names the directory. Returns
 * the origin, the language, the content, the administrator account and `stop()`, which ends the process and deletes
 * the directory.
 */
export async function launch({ language = 'en', content = 'fixture', label = language } = {}) {
  if (!['fixture', 'sample'].includes(content)) throw new Error(`unknown content ${content}`);
  for (const binary of [web, cli]) {
    if (!existsSync(binary)) {
      throw new Error(`${binary} is missing: run cargo build -p gono-forum-web --bins and npm run build first`);
    }
  }
  const port = await freePort();
  const dir = mkdtempSync(join(tmpdir(), `gonof-e2e-${process.pid}-${label}-`));
  // From here on a failed start undoes itself: `stop` ends the process once it runs and always removes the directory,
  // so a failing migration or fixture import leaves nothing behind either.
  let child = null;
  let ended = true;
  let exited = Promise.resolve();
  const stop = async () => {
    if (child && !ended) {
      child.kill('SIGTERM');
      await Promise.race([exited, new Promise((done) => setTimeout(done, 10000))]);
      if (!ended) child.kill('SIGKILL');
    }
    if (!process.env.GONOF_E2E_KEEP) rmSync(dir, { recursive: true, force: true });
  };
  const origin = `http://127.0.0.1:${port}`;
  try {
    const local = join(dir, 'local');
    mkdirSync(local);
    const database = join(dir, 'gonof.db');
    const config = join(dir, 'gono-forum.toml');
    writeFileSync(
      config,
      [
        `listen = "127.0.0.1:${port}"`,
        '[database]',
        `url = "sqlite://${database}"`,
        '[redis]',
        'enabled = false',
        '[common]',
        `encryption_key = "${hex()}"`,
        '[identity]',
        `local_directory = "${local}"`,
        '[identity.cookie]',
        `active_key = "${hex()}"`,
        '[identity.two_factor]',
        `active_key = "${hex()}"`,
        '[media]',
        `local_directory = "${join(dir, 'media')}"`,
        '',
      ].join('\n'),
    );
    const env = environment(config);
    const run = (command) => execFileSync(cli, [command], { cwd: root, env, stdio: 'pipe' });
    run('migrate');
    if (content === 'fixture') run('seed-demo');
    if (language !== 'en') {
      const db = new DatabaseSync(database);
      db.prepare("UPDATE gonof_common_settings SET svalue = ? WHERE skey = 'settings.site.language'").run(language);
      db.close();
    }

    const log = openSync(join(dir, 'web.log'), 'a');
    child = spawn(web, [], { cwd: root, env, stdio: ['ignore', log, log] });
    ended = false;
    exited = new Promise((done) => child.once('exit', done));
    exited.then(() => {
      ended = true;
    });
    await waitFor(async () => {
      if (ended) throw new Error(`the instance exited; see ${join(dir, 'web.log')}`);
      return (await fetch(`${origin}/healthz`)).status > 0;
    }, `${label} to listen`);
    const token = join(local, 'setup-token');
    await waitFor(async () => existsSync(token), `${label} setup token`);
    const form = new URLSearchParams({
      token: readFileSync(token, 'utf8').trim(),
      username: admin.username,
      email: admin.email,
      password: admin.password,
      password_confirm: admin.password,
      offset_minutes: '0',
      ...(content === 'sample' ? { sample_content: '1', sample_language: language } : {}),
    });
    const answer = await fetch(`${origin}/`, { method: 'POST', body: form, redirect: 'manual' });
    if (answer.status >= 400) throw new Error(`${label} setup wizard answered ${answer.status}`);
    await waitFor(async () => {
      const page = await fetch(`${origin}/login`);
      return page.status === 200 && (await page.text()).includes('name="_csrf"');
    }, `${label} to switch from the setup wizard to the forum`);
    if (content === 'sample') {
      // The wizard logs a failed sample import and finishes anyway; the checks need the content.
      const db = new DatabaseSync(database, { readOnly: true });
      const { threads } = db.prepare('SELECT COUNT(*) AS threads FROM gonof_forum_thread').get();
      db.close();
      if (threads !== 100) throw new Error(`${label} has ${threads} threads, not the sample's 100; see ${join(dir, 'web.log')}`);
    }
  } catch (error) {
    await stop();
    throw error;
  }
  return { origin, language, content, admin, dir, stop };
}
