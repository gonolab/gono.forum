// Measures what pages look like as computed numbers, for rewriting styles without reading the old source and for
// checking the rewrite afterwards. It starts a throwaway English instance (tests/e2e/support/instance.js), crawls the
// pages an anonymous visitor and the administrator can reach, and records for every element the computed styles that
// differ from the browser default for that tag, plus forced :hover / :focus-visible / :active states.
//
//   node scripts/ui-measure.mjs measure <label> [--skip=a,b] [--fast] → .local/ui-measure/<label>/
//       --skip lists built stylesheet sections (by file name, e.g. base,layout) to drop for a second variant;
//       the variant is measured next to the full one under <label>/without/.
//   node scripts/ui-measure.mjs report <label>                  → <label>/report.json and report.md
//   node scripts/ui-measure.mjs compare <before> <after>        → .local/ui-measure/compare-<before>-<after>.md
import { mkdirSync, readFileSync, writeFileSync, existsSync, readdirSync } from 'node:fs';
import { gunzipSync, gzipSync } from 'node:zlib';
import { join } from 'node:path';
import { chromium } from '@playwright/test';
import { launch, root } from '../tests/e2e/support/instance.js';

const out = join(root, '.local/ui-measure');
const [, , mode, ...rest] = process.argv;
const flag = (name) => rest.find((arg) => arg.startsWith(`--${name}=`))?.split('=')[1];
const positional = rest.filter((arg) => !arg.startsWith('--'));

const PROPS = [
  'display', 'position', 'top', 'right', 'bottom', 'left', 'float', 'clear', 'box-sizing', 'z-index',
  'width', 'height', 'min-width', 'max-width', 'min-height', 'max-height',
  'margin-top', 'margin-right', 'margin-bottom', 'margin-left',
  'padding-top', 'padding-right', 'padding-bottom', 'padding-left',
  'border-top-width', 'border-right-width', 'border-bottom-width', 'border-left-width',
  'border-top-style', 'border-right-style', 'border-bottom-style', 'border-left-style',
  'border-top-color', 'border-right-color', 'border-bottom-color', 'border-left-color',
  'border-top-left-radius', 'border-top-right-radius', 'border-bottom-right-radius', 'border-bottom-left-radius',
  'background-color', 'background-image', 'background-size', 'background-position', 'background-repeat',
  'color', 'font-family', 'font-size', 'font-weight', 'font-style', 'font-variant-numeric', 'line-height',
  'letter-spacing', 'text-align', 'text-transform', 'text-decoration-line', 'text-decoration-color',
  'text-underline-offset', 'text-indent', 'white-space', 'word-break', 'overflow-wrap', 'text-overflow',
  'overflow-x', 'overflow-y', 'vertical-align', '-webkit-line-clamp', '-webkit-box-orient',
  'flex-direction', 'flex-wrap', 'justify-content', 'align-items', 'align-content', 'align-self', 'justify-self',
  'flex-grow', 'flex-shrink', 'flex-basis', 'order', 'row-gap', 'column-gap',
  'grid-template-columns', 'grid-template-rows', 'grid-template-areas', 'grid-column-start', 'grid-column-end',
  'grid-row-start', 'grid-row-end', 'grid-auto-flow', 'grid-auto-columns', 'grid-auto-rows',
  'box-shadow', 'opacity', 'visibility', 'cursor', 'pointer-events', 'user-select', 'transform', 'transition',
  'outline-style', 'outline-width', 'outline-color', 'outline-offset', 'list-style-type', 'list-style-position',
  'object-fit', 'aspect-ratio', 'fill', 'stroke', 'stroke-width', 'filter', 'backdrop-filter', 'table-layout',
  'border-collapse', 'border-spacing', 'caret-color', 'accent-color', 'appearance', 'resize', 'content',
  'scroll-margin-top', 'inset-inline-start', 'tab-size', 'font-feature-settings', 'hyphens', 'isolation',
  'mix-blend-mode', 'clip-path', 'mask-image', 'animation-name', 'animation-duration',
];

// Runs in the page: records every element under body, keyed by its tag path (classes left out so that renamed
// classes still line up), with the computed values that differ from an unstyled element of the same tag.
function snapshot(props) {
  const frame = document.createElement('iframe');
  frame.style.cssText = 'position:absolute;width:0;height:0;border:0;visibility:hidden';
  document.body.appendChild(frame);
  const blank = frame.contentDocument;
  blank.open();
  blank.write('<!doctype html><html><head></head><body></body></html>');
  blank.close();
  const defaults = new Map();
  const base = (tag, ns) => {
    const key = `${ns}:${tag}`;
    if (!defaults.has(key)) {
      const probe = ns ? blank.createElementNS(ns, tag) : blank.createElement(tag);
      const host = ns ? blank.createElementNS('http://www.w3.org/2000/svg', 'svg') : blank.body;
      if (ns) blank.body.appendChild(host);
      host.appendChild(probe);
      const style = frame.contentWindow.getComputedStyle(probe);
      defaults.set(key, Object.fromEntries(props.map((name) => [name, style.getPropertyValue(name)])));
    }
    return defaults.get(key);
  };
  const pseudoProps = props.filter((name) => !['transition', 'cursor', 'user-select', 'pointer-events'].includes(name));
  const records = [];
  let index = 0;
  const walk = (element, path) => {
    const counts = new Map();
    for (const child of element.children) {
      if (child === frame || ['SCRIPT', 'STYLE', 'TEMPLATE', 'NOSCRIPT', 'LINK', 'META'].includes(child.tagName)) continue;
      const tag = child.tagName.toLowerCase();
      const n = (counts.get(tag) || 0) + 1;
      counts.set(tag, n);
      const key = `${path}/${tag}${n > 1 ? `[${n}]` : ''}`;
      const ns = child.namespaceURI === 'http://www.w3.org/1999/xhtml' ? '' : child.namespaceURI;
      const style = getComputedStyle(child);
      const unstyled = base(tag, ns);
      const values = {};
      for (const name of props) {
        const value = style.getPropertyValue(name);
        if (value !== unstyled[name]) values[name] = value;
      }
      for (const pseudo of ['::before', '::after', '::marker', '::placeholder']) {
        const ps = getComputedStyle(child, pseudo);
        const content = ps.getPropertyValue('content');
        if (pseudo !== '::placeholder' && pseudo !== '::marker' && (content === 'none' || content === 'normal')) continue;
        if (pseudo === '::placeholder' && !['INPUT', 'TEXTAREA'].includes(child.tagName)) continue;
        if (pseudo === '::marker' && style.getPropertyValue('display') !== 'list-item') continue;
        for (const name of pseudoProps) {
          const value = ps.getPropertyValue(name);
          if (value !== unstyled[name] && value !== style.getPropertyValue(name)) values[`${pseudo}${name}`] = value;
        }
      }
      const rect = child.getBoundingClientRect();
      const text = [...child.childNodes].filter((node) => node.nodeType === 3).map((node) => node.textContent).join(' ')
        .replace(/\s+/g, ' ').trim().slice(0, 40);
      child.setAttribute('data-ui-measure', String(index));
      records.push({
        i: index++,
        path: key,
        tag,
        classes: (child.getAttribute('class') || '').split(/\s+/).filter(Boolean),
        attrs: ['type', 'role', 'aria-current', 'aria-pressed', 'aria-expanded', 'hidden', 'open', 'disabled', 'data-active']
          .filter((name) => child.hasAttribute(name)).map((name) => `${name}=${child.getAttribute(name)}`),
        text,
        rect: [rect.x, rect.y + scrollY, rect.width, rect.height].map((v) => Math.round(v * 10) / 10),
        values,
      });
      walk(child, key);
    }
  };
  walk(document.body, 'body');
  frame.remove();
  const tokens = {};
  const probe = document.createElement('span');
  document.body.appendChild(probe);
  const rootStyle = getComputedStyle(document.documentElement);
  for (const sheet of [...document.styleSheets]) {
    let rules;
    try { rules = sheet.cssRules; } catch { continue; }
    for (const rule of rules) {
      if (!rule.style) continue;
      for (const name of rule.style) {
        if (!name.startsWith('--') || name in tokens) continue;
        const raw = rootStyle.getPropertyValue(name).trim();
        probe.style.color = '';
        probe.style.color = `var(${name})`;
        const color = getComputedStyle(probe).color;
        tokens[name] = { raw, color: probe.style.color ? color : null };
      }
    }
  }
  probe.remove();
  return { records, tokens, width: innerWidth, scrollWidth: document.documentElement.scrollWidth };
}

// Values of the element and its first descendants under one forced state, keeping only what changed.
function stateValues([props, ids]) {
  const result = {};
  for (const id of ids) {
    const element = document.querySelector(`[data-ui-measure="${id}"]`);
    if (!element) continue;
    const style = getComputedStyle(element);
    result[id] = Object.fromEntries(props.map((name) => [name, style.getPropertyValue(name)]));
    for (const pseudo of ['::before', '::after']) {
      const ps = getComputedStyle(element, pseudo);
      if (['none', 'normal'].includes(ps.getPropertyValue('content'))) continue;
      for (const name of props) result[id][`${pseudo}${name}`] = ps.getPropertyValue(name);
    }
  }
  return result;
}

const SKIP_PATH = /\/(logout|api|static|media|thumb|avatar|brand|forum-icon)(\/|$)|\.xml$|\/feeds\/private|\/download$|\/(delete|revoke|retry|cancel|withdraw|restore|rotate|disable|enable|read-all|apply)$/;

function shape(url) {
  const { pathname, search } = new URL(url);
  const keys = [...new URLSearchParams(search).keys()].sort().join('&');
  return pathname.replace(/\d+/g, '{n}') + (keys ? `?${keys}` : '');
}

async function crawl(context, origin, seeds, limit) {
  const page = await context.newPage();
  const seen = new Map();
  const queue = seeds.map((path) => `${origin}${path}`);
  const visited = new Set();
  while (queue.length && seen.size < limit) {
    const url = queue.shift();
    if (visited.has(url)) continue;
    visited.add(url);
    const key = shape(url);
    const bucket = seen.get(key) || [];
    if (bucket.length >= 2) continue;
    let response;
    try {
      response = await page.goto(url, { waitUntil: 'domcontentloaded', timeout: 20000 });
    } catch {
      continue;
    }
    if (!response || !(response.headers()['content-type'] || '').includes('text/html')) continue;
    const final = page.url();
    if (!final.startsWith(origin) || shape(final) !== key && seen.has(shape(final))) continue;
    bucket.push(url.slice(origin.length));
    seen.set(key, bucket);
    const links = await page.$$eval('a[href]', (anchors) => anchors.map((anchor) => anchor.href));
    for (const link of links) {
      if (!link.startsWith(origin)) continue;
      const clean = link.split('#')[0];
      const path = new URL(clean).pathname;
      if (SKIP_PATH.test(path)) continue;
      if (!visited.has(clean)) queue.push(clean);
    }
  }
  await page.close();
  return [...seen.values()].flat();
}

function sections(skip) {
  const css = readFileSync(join(root, 'apps/web/static/css/forum.css'), 'utf8');
  if (!skip.length) return css;
  const parts = css.split(/(?=^\/\* (?:forum:themes|apps\/web\/frontend\/styles\/[\w.-]+\.css) \*\/$)/m);
  return parts.filter((part) => {
    const name = part.match(/^\/\* apps\/web\/frontend\/styles\/([\w.-]+)\.css \*\//)?.[1];
    return !name || !skip.includes(name);
  }).join('');
}

async function measurePage(context, origin, path, options) {
  const page = await context.newPage();
  if (options.css) {
    await page.route('**/static/css/forum.css*', (route) => route.fulfill({ contentType: 'text/css', body: options.css }));
  }
  const response = await page.goto(`${origin}${path}`, { waitUntil: 'networkidle', timeout: 30000 });
  await page.evaluate(() => document.fonts.ready);
  if (options.open) {
    await page.evaluate(() => {
      for (const details of document.querySelectorAll('details')) details.open = true;
    });
  }
  const data = await page.evaluate(snapshot, PROPS);
  data.status = response.status();
  if (options.states) {
    const interactive = data.records.filter((record) =>
      ['a', 'button', 'input', 'select', 'textarea', 'summary', 'label'].includes(record.tag)
      || record.attrs.some((attr) => attr.startsWith('role=')) || record.classes.length);
    const picked = new Map();
    for (const record of interactive) {
      const key = `${record.tag}.${record.classes.join('.')}`;
      if (!picked.has(key) && record.rect[2] > 0) picked.set(key, record);
    }
    const client = await context.newCDPSession(page);
    await client.send('DOM.enable');
    await client.send('CSS.enable');
    const { root: documentNode } = await client.send('DOM.getDocument', { depth: 0 });
    const normal = await page.evaluate(stateValues, [PROPS, [...picked.values()].flatMap((record) =>
      [record.i, ...data.records.filter((other) => other.path.startsWith(`${record.path}/`)).slice(0, 12).map((other) => other.i)])]);
    data.states = {};
    for (const state of ['hover', 'focus-visible', 'active']) {
      const changed = {};
      for (const record of picked.values()) {
        const { nodeId } = await client.send('DOM.querySelector', { nodeId: documentNode.nodeId, selector: `[data-ui-measure="${record.i}"]` });
        if (!nodeId) continue;
        await client.send('CSS.forcePseudoState', { nodeId, forcedPseudoClasses: [state] });
        const ids = [record.i, ...data.records.filter((other) => other.path.startsWith(`${record.path}/`)).slice(0, 12).map((other) => other.i)];
        const values = await page.evaluate(stateValues, [PROPS, ids]);
        await client.send('CSS.forcePseudoState', { nodeId, forcedPseudoClasses: [] });
        for (const [id, props] of Object.entries(values)) {
          const diff = Object.fromEntries(Object.entries(props).filter(([name, value]) => normal[id]?.[name] !== value
            && !name.startsWith('transition')));
          if (Object.keys(diff).length) (changed[record.i] ||= {})[id] = diff;
        }
      }
      data.states[state] = changed;
    }
    await client.detach();
  }
  await page.close();
  return data;
}

async function measure(label) {
  const skip = (flag('skip') || '').split(',').filter(Boolean);
  // --fast: closed details only and no forced states, for quick before/after comparisons.
  const fast = rest.includes('--fast');
  const instance = await launch({ language: 'en', label: 'measure' });
  const browser = await chromium.launch();
  try {
    const anonymous = await browser.newContext({ locale: 'en-US', timezoneId: 'UTC', reducedMotion: 'reduce' });
    const anonymousPaths = await crawl(anonymous, instance.origin, ['/', '/forums', '/login', '/register', '/terms', '/rules', '/privacy', '/contact', '/ranks', '/search?q=Rust', '/no-such-page-e2e'], 80);
    // The administrator signs in through the form once; the session is reused by every context.
    const signer = await browser.newContext();
    const page = await signer.newPage();
    await page.goto(`${instance.origin}/login`);
    await page.fill('form[action$="/login"] input[name="username"]', instance.admin.username);
    await page.fill('form[action$="/login"] input[name="password"]', instance.admin.password);
    await Promise.all([page.waitForURL((url) => !url.pathname.endsWith('/login')), page.click('form[action$="/login"] [type="submit"]')]);
    const state = await signer.storageState();
    await signer.close();
    const admin = await browser.newContext({ storageState: state, locale: 'en-US', timezoneId: 'UTC', reducedMotion: 'reduce' });
    const adminPaths = await crawl(admin, instance.origin, ['/my', '/manage', '/new', '/thread-104', '/space/1', '/my/account', '/my/profile', '/my/messages'], 200);
    await anonymous.close();
    await admin.close();
    const variants = [{ name: 'full', css: null }];
    if (skip.length) variants.push({ name: 'without', css: sections(skip) });
    const views = [
      { name: 'desktop', viewport: { width: 1440, height: 900 }, colorScheme: 'light', states: true },
      { name: 'mobile', viewport: { width: 390, height: 844 }, colorScheme: 'light', isMobile: true, hasTouch: true },
      { name: 'desktop-dark', viewport: { width: 1440, height: 900 }, colorScheme: 'dark' },
    ];
    const jobs = [];
    for (const variant of variants) {
      for (const view of views) {
        if (variant.name !== 'full' && view.name === 'desktop-dark') continue;
        for (const [who, paths] of [['anonymous', anonymousPaths], ['admin', adminPaths]]) {
          for (const path of paths) jobs.push({ variant, view, who, path });
        }
      }
    }
    const dir = join(out, label);
    mkdirSync(dir, { recursive: true });
    writeFileSync(join(dir, 'pages.json'), JSON.stringify({ anonymous: anonymousPaths, admin: adminPaths, skip }, null, 2));
    let done = 0;
    const worker = async () => {
      while (jobs.length) {
        const job = jobs.shift();
        const context = await browser.newContext({
          viewport: job.view.viewport, colorScheme: job.view.colorScheme, isMobile: job.view.isMobile,
          hasTouch: job.view.hasTouch, locale: 'en-US', timezoneId: 'UTC', reducedMotion: 'reduce',
          storageState: job.who === 'admin' ? state : undefined,
        });
        for (const open of fast ? [false] : [false, true]) {
          try {
            const data = await measurePage(context, instance.origin, job.path, { css: job.variant.css, open, states: !fast && job.view.states && !open });
            const name = `${job.who}${job.path.replace(/[^\w-]+/g, '_')}${open ? '.open' : ''}.json.gz`;
            const target = join(dir, job.variant.name, job.view.name);
            mkdirSync(target, { recursive: true });
            writeFileSync(join(target, name), gzipSync(JSON.stringify({ path: job.path, who: job.who, open, ...data })));
          } catch (error) {
            console.error(`${job.variant.name} ${job.view.name} ${job.who} ${job.path}: ${error.message}`);
          }
        }
        await context.close();
        if (++done % 25 === 0) console.log(`${done} pages measured, ${jobs.length} left`);
      }
    };
    await Promise.all([worker(), worker(), worker(), worker()]);
    console.log(`measured ${anonymousPaths.length} anonymous and ${adminPaths.length} administrator pages into ${dir}`);
  } finally {
    await browser.close();
    await instance.stop();
  }
}

function load(dir) {
  const result = [];
  if (!existsSync(dir)) return result;
  for (const name of readdirSync(dir).sort()) result.push({ name, ...JSON.parse(gunzipSync(readFileSync(join(dir, name)))) });
  return result;
}

const signature = (record) => `${record.tag}${record.classes.length ? `.${[...record.classes].sort().join('.')}` : ''}`;

// Inherited properties are only reported where they differ from the parent element, and the colour properties that
// default to currentColor only where they differ from the element's own colour: that keeps what the element sets.
const INHERITED = new Set(['color', 'font-family', 'font-size', 'font-weight', 'font-style', 'font-variant-numeric',
  'line-height', 'letter-spacing', 'text-align', 'text-transform', 'text-indent', 'white-space', 'word-break',
  'overflow-wrap', 'list-style-type', 'list-style-position', 'visibility', 'cursor', 'caret-color', 'fill', 'stroke',
  'stroke-width', 'border-collapse', 'border-spacing', 'tab-size', 'font-feature-settings', 'hyphens', 'user-select',
  'pointer-events', 'accent-color', 'text-underline-offset']);
const CURRENT_COLOR = ['border-top-color', 'border-right-color', 'border-bottom-color', 'border-left-color',
  'outline-color', 'text-decoration-color', 'caret-color', 'column-rule-color'];

function own(record, parent) {
  const values = {};
  for (const [name, value] of Object.entries(record.values)) {
    const bare = name.replace(/^::\w+/, '');
    if (name === bare && INHERITED.has(name) && parent && (parent.values[name] ?? null) === value) continue;
    if (CURRENT_COLOR.includes(bare) && value === (record.values[name.replace(bare, 'color')] ?? record.values.color)) continue;
    if (/^border-(top|right|bottom|left)-color$/.test(name)) {
      const side = name.split('-')[1];
      if (!record.values[`border-${side}-width`] || record.values[`border-${side}-style`] === undefined) continue;
    }
    if (name === 'outline-color' && !record.values['outline-style']) continue;
    if (name === 'text-decoration-color' && !record.values['text-decoration-line']) continue;
    values[name] = value;
  }
  return values;
}

function tokenNames(pages) {
  const map = new Map();
  for (const page of pages.slice(0, 3)) {
    for (const [name, token] of Object.entries(page.tokens || {})) {
      if (token.color && !/^rgba\(0, 0, 0, 0\)$/.test(token.color)) {
        const list = map.get(token.color) || new Set();
        list.add(name);
        map.set(token.color, list);
      }
    }
  }
  return map;
}

const named = (value, tokens) => value.replace(/rgba?\([^)]*\)|color\(srgb[^)]*\)/g, (color) => {
  const list = tokens.get(color);
  return list ? `${color}{${[...list].slice(0, 3).join('|')}}` : color;
});

// Groups elements by tag and classes (class-less elements by their parent's group plus tag); for each group lists,
// per view, the values the element sets itself, marking with * those that the skipped stylesheet sections contribute.
function report(label) {
  const dir = join(out, label);
  const groups = new Map();
  const views = readdirSync(join(dir, 'full')).map((view) => [view, join(dir, 'full', view)]);
  if (existsSync(join(dir, 'widths'))) {
    for (const width of readdirSync(join(dir, 'widths'))) views.push([`w${width}`, join(dir, 'widths', width)]);
  }
  for (const [view, source] of views) {
    const full = load(source);
    const tokens = tokenNames(full);
    const without = new Map(load(join(dir, 'without', view)).map((page) => [page.name, page]));
    for (const page of full) {
      const other = without.get(page.name);
      const byPath = new Map(page.records.map((record) => [record.path, record]));
      const otherByPath = new Map((other?.records || []).map((record) => [record.path, record]));
      const keyOf = new Map();
      for (const record of page.records) {
        const parentPath = record.path.slice(0, record.path.lastIndexOf('/'));
        const parentKey = keyOf.get(parentPath);
        const key = record.classes.length || !parentKey ? signature(record) : `${parentKey.split(' > ').slice(-1)[0]} > ${record.tag}`;
        keyOf.set(record.path, key);
        const group = groups.get(key) || { count: 0, pages: new Set(), views: {} };
        group.count++;
        if (group.pages.size < 6) group.pages.add(`${page.who}:${page.path}`);
        const bucket = (group.views[view] ||= {});
        const mine = own(record, byPath.get(parentPath));
        const base = otherByPath.get(record.path);
        const baseOwn = base ? own(base, otherByPath.get(parentPath)) : null;
        for (const [name, value] of Object.entries(mine)) {
          const entry = (bucket[name] ||= { values: {}, derived: 0, checked: 0 });
          const shown = named(value, tokens);
          entry.values[shown] = (entry.values[shown] || 0) + 1;
          if (baseOwn) {
            entry.checked++;
            if (baseOwn[name] !== value) entry.derived++;
          }
        }
        if (page.states && view === 'desktop') {
          for (const [state, changed] of Object.entries(page.states)) {
            const diffs = changed[record.i];
            if (!diffs) continue;
            const states = (group.states ||= {});
            const list = (states[state] ||= []);
            if (list.length < 2) list.push(Object.fromEntries(Object.entries(diffs).map(([id, diff]) => {
              const target = page.records.find((r) => String(r.i) === id);
              const filtered = Object.fromEntries(Object.entries(diff).filter(([name]) => !CURRENT_COLOR.includes(name)).map(([name, value]) => [name, named(value, tokens)]));
              return [target ? (target === record ? 'self' : signature(target)) : id, filtered];
            }).filter(([, diff]) => Object.keys(diff).length)));
          }
        }
        groups.set(key, group);
      }
    }
  }
  const json = Object.fromEntries([...groups].map(([key, group]) => [key, { ...group, pages: [...group.pages] }]));
  writeFileSync(join(dir, 'report.json'), JSON.stringify(json));
  const lines = [`# Measured appearance: ${label}`, '',
    'Values each element sets itself (desktop light); the mobile and dark lines list only what differs. * marks values',
    'the skipped stylesheet sections contribute.', ''];
  const NOISE = (name, value) => (name === 'box-sizing' && value === 'border-box') || (/^min-(width|height)$/.test(name) && value === 'auto')
    || (name === 'transition' && value === 'none');
  const top = (entry) => Object.entries(entry.values).sort((a, b) => b[1] - a[1]).slice(0, 3)
    .map(([v, n]) => `${v}${n > 1 ? ` ×${n}` : ''}`).join(' | ');
  const first = (entry) => Object.entries(entry.values).sort((a, b) => b[1] - a[1])[0]?.[0];
  for (const [key, group] of [...groups].sort((a, b) => a[0].localeCompare(b[0]))) {
    const rows = [];
    const desktop = group.views.desktop || {};
    const keep = Object.entries(desktop).filter(([name, entry]) => !NOISE(name, first(entry)));
    if (keep.length) rows.push(...keep.map(([name, entry]) => `- ${entry.derived ? '*' : ' '}${name}: ${top(entry)}`));
    for (const view of ['mobile', 'desktop-dark']) {
      const props = group.views[view] || {};
      const diff = Object.entries(props).filter(([name, entry]) => !NOISE(name, first(entry)) && first(entry) !== (desktop[name] && first(desktop[name])));
      const gone = Object.keys(desktop).filter((name) => !(name in props) && !NOISE(name, first(desktop[name])));
      if (diff.length || gone.length) {
        rows.push(`- ${view}: ${diff.map(([name, entry]) => `${entry.derived ? '*' : ''}${name}: ${top(entry)}`).join('; ')}${gone.length ? `${diff.length ? '; ' : ''}unset: ${gone.join(', ')}` : ''}`);
      }
    }
    if (!rows.length && !group.states) continue;
    lines.push(`## ${key}`, '', `${group.count} elements; ${[...group.pages].slice(0, 3).join(', ')}`, '', ...rows);
    for (const [state, list] of Object.entries(group.states || {})) if (list[0] && Object.keys(list[0]).length) lines.push(`- :${state} ${JSON.stringify(list[0])}`);
    lines.push('');
  }
  writeFileSync(join(dir, 'report.md'), `${lines.join('\n')}\n`);
  console.log(`${groups.size} element groups; report in ${join(dir, 'report.md')}`);
}

// Element-by-element comparison of two full measurements by tag path; lists the properties that changed. Values that
// render the same (a 0px 0px background position, a flex-basis on a non-flex item), the insides of SVG icons and
// elements that are not rendered in either measurement are left out; position changes of more than 2px are listed
// separately, since a wider label upstream moves everything after it.
const SAME = new Set(['background-position', 'background-size', 'background-repeat', 'appearance', 'transition',
  'animation-name', 'animation-duration', 'flex-basis', 'width', 'height', 'fill', 'stroke', 'stroke-width', 'min-width',
  'min-height', 'caret-color', 'text-decoration-color', 'outline-color']);
function compare(before, after) {
  const lines = [`# ${before} → ${after}`, ''];
  const tally = new Map();
  const moves = new Map();
  let total = 0;
  for (const view of readdirSync(join(out, before, 'full'))) {
    const next = new Map(load(join(out, after, 'full', view)).map((page) => [page.name, page]));
    for (const page of load(join(out, before, 'full', view))) {
      const other = next.get(page.name);
      if (!other) continue;
      const byPath = new Map(other.records.map((record) => [record.path, record]));
      const hidden = (record) => record.values.display === 'none' || record.rect[2] === 0 && record.rect[3] === 0;
      const diffs = [];
      for (const record of page.records) {
        if (/\/svg(\[\d+\])?\//.test(record.path)) continue;
        const twin = byPath.get(record.path);
        if (!twin) {
          if (!hidden(record)) diffs.push(`  - ${record.path} (${signature(record)}) missing`);
          continue;
        }
        if (hidden(record) && hidden(twin)) continue;
        const names = new Set([...Object.keys(record.values), ...Object.keys(twin.values)]);
        const changed = [...names].filter((name) => record.values[name] !== twin.values[name]
          && !SAME.has(name.replace(/^::\w+/, '')) && !(name.endsWith('color') && /^border-\w+-color$/.test(name)
            && !(twin.values[name.replace('color', 'width')] || record.values[name.replace('color', 'width')])));
        const moved = record.rect.some((value, index) => Math.abs(value - twin.rect[index]) > 2);
        const sig = signature(twin);
        for (const name of changed) {
          const key = `${sig} | ${name}: ${record.values[name] ?? '(default)'} → ${twin.values[name] ?? '(default)'}`;
          const entry = tally.get(key) || { count: 0, pages: new Set() };
          entry.count++;
          entry.pages.add(`${view} ${page.name.replace(/\.json\.gz$/, '')}`);
          tally.set(key, entry);
        }
        if (moved) {
          const [x, y, w, h] = record.rect;
          const [x2, y2, w2, h2] = twin.rect;
          if (Math.abs(w - w2) > 2 || Math.abs(h - h2) > 2) {
            const key = `${sig} | size ${Math.round(w)}×${Math.round(h)} → ${Math.round(w2)}×${Math.round(h2)}`;
            const entry = moves.get(key) || { count: 0, pages: new Set() };
            entry.count++;
            entry.pages.add(`${view} ${page.name.replace(/\.json\.gz$/, '')}`);
            moves.set(key, entry);
          }
        }
        if (changed.length) {
          diffs.push(`  - ${record.path} ${signature(record)} → ${sig}`
            + changed.map((name) => `\n    ${name}: ${record.values[name] ?? '(default)'} → ${twin.values[name] ?? '(default)'}`).join(''));
        }
      }
      if (diffs.length) {
        total += diffs.length;
        lines.push(`## ${view} ${page.name} (${diffs.length})`, ...diffs, '');
      }
    }
  }
  const top = (map, limit) => [...map].sort((a, b) => b[1].count - a[1].count).slice(0, limit)
    .map(([key, entry]) => `- ${entry.count} × ${key}   [${[...entry.pages].slice(0, 3).join('; ')}]`);
  const target = join(out, `compare-${before}-${after}.md`);
  writeFileSync(target, `${['# Most frequent property changes', '', ...top(tally, 600), '', '# Most frequent size changes', '',
    ...top(moves, 300), '', ...lines].join('\n')}\n`);
  console.log(`${total} elements with property changes, ${tally.size} distinct changes; see ${target}`);
}

// Finds the viewport widths where layout switches: for a few pages, a per-element layout signature is taken at
// widths 320–1600 and every change is narrowed down to the exact pixel.
function layoutSignature() {
  const parts = [];
  for (const element of document.body.querySelectorAll('*')) {
    if (['SCRIPT', 'STYLE', 'TEMPLATE', 'NOSCRIPT'].includes(element.tagName)) continue;
    const s = getComputedStyle(element);
    const cls = [...element.classList].sort().join('.');
    parts.push(`${element.tagName}.${cls}|${s.display}|${s.position}|${s.flexDirection}|${s.flexWrap}|`
      + `${s.gridTemplateColumns.split(' ').length}|${s.float}|${s.visibility}|${s.fontSize}|${s.gap}|${s.textAlign}`);
  }
  return parts;
}

async function sweep(label) {
  const instance = await launch({ language: 'en', label: 'sweep' });
  const browser = await chromium.launch();
  const paths = ['/', '/forums', '/showcase', '/thread-103', '/login', '/space/2', '/terms', '/search?q=Rust'];
  const result = {};
  try {
    for (const [mobile, path] of paths.flatMap((path) => [[false, path], [true, path]])) {
      const context = await browser.newContext({ viewport: { width: 1600, height: 900 }, isMobile: mobile, hasTouch: mobile, reducedMotion: 'reduce' });
      const page = await context.newPage();
      await page.goto(`${instance.origin}${path}`, { waitUntil: 'networkidle' });
      const at = async (width) => {
        await page.setViewportSize({ width, height: 900 });
        return page.evaluate(layoutSignature);
      };
      const changes = [];
      let previous = await at(320);
      for (let width = 330; width <= 1600; width += 10) {
        const current = await at(width);
        if (current.join('\n') !== previous.join('\n')) {
          let low = width - 10;
          let high = width;
          const lowSig = previous.join('\n');
          while (high - low > 1) {
            const mid = Math.floor((low + high) / 2);
            if ((await at(mid)).join('\n') === lowSig) low = mid; else high = mid;
          }
          const before = await at(low);
          const after = await at(high);
          const changed = [];
          for (let index = 0; index < Math.max(before.length, after.length); index++) {
            if (before[index] !== after[index]) changed.push(`${before[index] ?? '-'}  →  ${after[index] ?? '-'}`);
          }
          changes.push({ width: high, changed: [...new Set(changed)].slice(0, 30) });
          await at(width);
        }
        previous = current;
      }
      result[`${mobile ? 'mobile' : 'desktop'} ${path}`] = changes;
      await context.close();
    }
  } finally {
    await browser.close();
    await instance.stop();
  }
  const dir = join(out, label);
  mkdirSync(dir, { recursive: true });
  writeFileSync(join(dir, 'sweep.json'), JSON.stringify(result, null, 1));
  const widths = {};
  for (const changes of Object.values(result)) for (const change of changes) widths[change.width] = (widths[change.width] || 0) + 1;
  console.log('layout switches at widths (pages):', Object.entries(widths).sort((a, b) => a[0] - b[0]).map(([w, n]) => `${w}(${n})`).join(' '));
}

// Measures a fixed set of pages at in-between widths (light, full styles only), so each breakpoint's values are
// known and not only the 1440 and 390 ends.
async function widths(label) {
  const list = (flag('widths') || '1100,950,800,720,660,560,450').split(',').map(Number);
  const instance = await launch({ language: 'en', label: 'widths' });
  const browser = await chromium.launch();
  try {
    const signer = await browser.newContext();
    const page = await signer.newPage();
    await page.goto(`${instance.origin}/login`);
    await page.fill('form[action$="/login"] input[name="username"]', instance.admin.username);
    await page.fill('form[action$="/login"] input[name="password"]', instance.admin.password);
    await Promise.all([page.waitForURL((url) => !url.pathname.endsWith('/login')), page.click('form[action$="/login"] [type="submit"]')]);
    const state = await signer.storageState();
    await signer.close();
    const paths = [
      ['anonymous', '/'], ['anonymous', '/forums'], ['anonymous', '/showcase'], ['anonymous', '/thread-103'],
      ['anonymous', '/login'], ['anonymous', '/register'], ['anonymous', '/space/2'], ['anonymous', '/terms'],
      ['anonymous', '/ranks'], ['anonymous', '/search?q=Rust'], ['admin', '/'], ['admin', '/thread-104'], ['admin', '/new'],
      ['admin', '/my'], ['admin', '/my/account'], ['admin', '/my/profile'], ['admin', '/manage'], ['admin', '/manage/settings'],
      ['admin', '/manage/users'], ['admin', '/my/notifications'], ['admin', '/my/messages'],
    ];
    const jobs = list.flatMap((width) => paths.map(([who, path]) => ({ width, who, path })));
    const worker = async () => {
      while (jobs.length) {
        const job = jobs.shift();
        const context = await browser.newContext({ viewport: { width: job.width, height: 900 }, locale: 'en-US', timezoneId: 'UTC', reducedMotion: 'reduce', storageState: job.who === 'admin' ? state : undefined });
        try {
          const data = await measurePage(context, instance.origin, job.path, {});
          const target = join(out, label, 'widths', String(job.width));
          mkdirSync(target, { recursive: true });
          writeFileSync(join(target, `${job.who}${job.path.replace(/[^\w-]+/g, '_')}.json.gz`), gzipSync(JSON.stringify({ path: job.path, who: job.who, ...data })));
        } catch (error) {
          console.error(`${job.width} ${job.who} ${job.path}: ${error.message}`);
        }
        await context.close();
      }
    };
    await Promise.all([worker(), worker(), worker(), worker()]);
  } finally {
    await browser.close();
    await instance.stop();
  }
  console.log(`measured ${list.length} widths`);
}

if (mode === 'measure') await measure(positional[0] || 'latest');
else if (mode === 'widths') await widths(positional[0] || 'latest');
else if (mode === 'sweep') await sweep(positional[0] || 'latest');
else if (mode === 'report') report(positional[0] || 'latest');
else if (mode === 'compare') compare(positional[0], positional[1]);
else {
  console.error('usage: node scripts/ui-measure.mjs measure <label> [--skip=a,b] | report <label> | compare <before> <after>');
  process.exit(2);
}
