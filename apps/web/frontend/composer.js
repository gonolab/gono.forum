import { enhancePoll } from './composer-poll.js';
import { enhanceDeadline } from './composer-deadline.js';
import { enhanceLotteryPrizes } from './lottery-prizes.js';
import { parser as commonmark, GFM, Subscript, Superscript, Emoji } from '@lezer/markdown';
import { bindPacketPreview } from './red-packet.js';

// Interface text comes from the language's script catalogue (docs/architecture.md, "Languages"), loaded before this
// bundle.
const t = (key, args) => (window.gonofT ? window.gonofT(key, args) : key);
const forms = new Map();
// Path prefix of a sub-path deployment (e.g. /bbs), an empty string for a root deployment; the page writes it on
// <html data-base>.
const base = () => document.documentElement.dataset.base || '';
// The address-bar path minus the prefix is handed to the server as the on-site return address; the server adds the
// prefix back when it issues the redirect.
const sitePath = path => {
  const prefix = base();
  return prefix && (path === prefix || path.startsWith(`${prefix}/`)) ? path.slice(prefix.length) || '/' : path;
};
const MAX_AGE = 7 * 24 * 60 * 60 * 1000;
const IMAGE_TYPES = 'image/jpeg,image/png,image/webp,image/gif';
const labels = () => [
  ['bold', t('js-editor-bold')], ['italic', t('js-editor-italic')], ['strike', t('js-editor-strike')], ['heading', t('js-editor-heading')],
  ['quote', t('js-editor-quote')], ['ul', t('js-editor-ul')], ['ol', t('js-editor-ol')], ['code', t('js-editor-code')],
  ['link', t('js-editor-link')], ['attach', t('js-editor-attach')], ['spoiler', t('js-editor-spoiler')],
  ['emoji', t('js-editor-emoji')], ['mention', t('js-editor-mention')], ['preview', t('js-editor-preview')],
  ['fullscreen', t('js-editor-fullscreen')],
];
// Markdown syntax tree of the body: used to find references in the body when an attachment is deleted, and to avoid
// code blocks when Enter continues a list.
const markdown = commonmark.configure([GFM, Subscript, Superscript, Emoji]);
// Line-start markup continued on Enter: quote `>` (may nest), optionally followed by unordered `-` `*` `+` or ordered
// `1.` `1)`, optionally with a task box.
const LINE_MARKUP = /^((?:[ \t]*>[ \t]?)*)(?:([ \t]*)(?:([-+*])|(\d{1,9})([.)]))([ \t]+)(\[[ xX]\][ \t]+)?)?/;
const THEMATIC_BREAK = /^ {0,3}([-*_])(?:[ \t]*\1){2,}[ \t]*$/;
const VERBATIM_BLOCKS = new Set(['FencedCode', 'CodeBlock', 'HTMLBlock', 'CommentBlock', 'ProcessingInstructionBlock']);

function element(tag, attrs = {}, text = '') {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(attrs)) node.setAttribute(key, value);
  node.textContent = text;
  return node;
}
function button(label, action) {
  return element('button', { type: 'button', title: label, 'aria-label': label, 'data-editor-action': action }, label);
}
// The lottery redemption codes (lottery_codes) are deliberately not here: redemption codes must not stay in the
// browser's local draft.
const ACTIVITY_FIELDS = ['category', 'budget', 'shares', 'distribution', 'deadline', 'max_choices', 'options', 'source_name', 'source_url', 'source_author', 'entry_mode', 'requirements', 'review', 'early_draw', 'prizes'];
// Empty values do not override server defaults (e.g. the deadline defaults to three days later).
function setActivityValue(field, value) {
  if (!field || typeof value !== 'string') return;
  if (field.type === 'checkbox') {
    field.checked = value === field.value;
    return;
  }
  if (value === '') return;
  if (field instanceof HTMLSelectElement && ![...field.options].some(option => option.value === value)) return;
  field.value = value;
}
// Drafts are kept per forum: forums under different paths of one host share the browser's storage, and the same uid
// on two of them is two different members. Keys without the path, from before it was added, are never read: which
// forum wrote them cannot be told.
function storageKey(uid, draft) {
  return `gonof:${encodeURIComponent(base() || '/')}:draft:${encodeURIComponent(uid)}:${encodeURIComponent(draft)}`;
}
function requestKey() {
  return [...crypto.getRandomValues(new Uint8Array(32))].map(value => value.toString(16).padStart(2, '0')).join('');
}
function responseMessage(html, fallback) {
  const doc = new DOMParser().parseFromString(html, 'text/html');
  const message = doc.querySelector('[data-error-message], [role="alert"]')?.textContent.trim();
  return message ? message.slice(0, 500) : fallback;
}
function removeDraft(uid, draft, requestKey) {
  try {
    const key = storageKey(uid, draft);
    if (requestKey && JSON.parse(localStorage.getItem(key))?.request_key !== requestKey) return;
    localStorage.removeItem(key);
  } catch { /* Storage may be disabled. */ }
}
function textChange(before, after) {
  let from = 0;
  while (from < before.length && from < after.length && before[from] === after[from]) from++;
  let oldEnd = before.length;
  let newEnd = after.length;
  while (oldEnd > from && newEnd > from && before[oldEnd - 1] === after[newEnd - 1]) { oldEnd--; newEnd--; }
  return position => position < from ? position : position >= oldEnd ? position + newEnd - oldEnd : newEnd;
}

function attachmentRanges(source, aid) {
  const tree = markdown.parse(source);
  const label = value => value.replace(/\\([!"#$%&'()*+,\-./:;<=>?@[\]^_`{|}~\\])/g, '$1').trim().replace(/\s+/g, ' ').toLowerCase();
  const destination = value => {
    value = value.replace(/^<|>$/g, '').replace(/\\([:])/g, '$1').replace(/&#(\d+);|&#x([0-9a-f]+);|&colon;/gi, (_, dec, hex) => dec || hex ? String.fromCodePoint(Math.min(0x10ffff, Number.parseInt(dec || hex, dec ? 10 : 16))) : ':');
    const match = value.match(/^attachment:(\d+)$/);
    return match && BigInt(match[1]) === BigInt(aid);
  };
  const references = new Set();
  const ranges = [];
  tree.iterate({ enter(ref) {
    if (ref.name !== 'LinkReference') return;
    const node = ref.node, url = node.getChild('URL'), name = node.getChild('LinkLabel');
    if (url && name && destination(source.slice(url.from, url.to))) {
      references.add(label(source.slice(name.from + 1, name.to - 1)));
      ranges.push({ from: node.from, to: node.to });
    }
  } });
  tree.iterate({ enter(ref) {
    if (!['Image', 'Link'].includes(ref.name)) return;
    const node = ref.node, url = node.getChild('URL');
    if (url && destination(source.slice(url.from, url.to))) { ranges.push({ from: node.from, to: node.to }); return false; }
    if (url) return;
    const explicit = node.getChild('LinkLabel');
    let name = explicit ? source.slice(explicit.from + 1, explicit.to - 1) : '';
    if (!name) {
      const end = node.getChildren('LinkMark').find(mark => source.slice(mark.from, mark.to) === ']');
      if (end) name = source.slice(node.from + (ref.name === 'Image' ? 2 : 1), end.from);
    }
    if (references.has(label(name))) { ranges.push({ from: node.from, to: node.to }); return false; }
  } });
  return ranges.sort((a, b) => b.from - a.from);
}

class Composer {
  constructor(form) {
    this.form = form;
    this.textarea = form.querySelector('textarea[name="body"]');
    if (!this.textarea) return;
    this.abort = new AbortController();
    this.media = new Map();
    this.markers = new Set();
    this.draftKey = storageKey(form.dataset.uid, form.dataset.draft);
    // Only an editor the author has changed writes the local draft (`touch`): leaving an untouched one, such as a
    // second tab of the same thread or a new-thread page opened from another forum, must not replace the draft saved
    // elsewhere with its empty or server-filled fields.
    this.dirty = false;
    this.lastBody = this.textarea.value;
    this.originalRequired = this.textarea.required;
    this.container = element('div', { class: 'composer-shell' });
    this.textarea.before(this.container);
    this.container.append(this.textarea);
    if (!this.textarea.placeholder) this.textarea.placeholder = t('js-editor-placeholder');
    this.toolbar =form.querySelector('[data-editor-toolbar]') || element('div', { 'data-editor-toolbar': '' });
    this.toolbar.classList.add('write-toolbar');
    this.toolbar.setAttribute('role', 'toolbar');
    this.toolbar.setAttribute('aria-label', t('js-editor-toolbar'));
    this.container.prepend(this.toolbar);
    if (!this.toolbar.children.length) {
      for (const [action, label] of labels()) {
        const control = button(label, action);
        control.className = `tool-${action}`;
        if (action === 'emoji') control.textContent = '☺';
        else if (action === 'mention') control.textContent = '@';
        else control.textContent = '';
        this.toolbar.append(control);
      }
    }
    this.preview = form.querySelector('[data-editor-preview]') || element('div', { 'data-editor-preview': '', class: 'prose write-preview', 'aria-live': 'polite' });
    this.preview.hidden = true;
    this.container.append(this.preview);
    this.picker = element('div', { class: 'emoji-menu', 'data-editor-picker': '' });
    this.picker.hidden = true;
    this.toolbar.after(this.picker);
    this.status = form.querySelector('[data-editor-status]') || element('div', { 'data-editor-status': '', class: 'write-status', role: 'status' });
    this.container.after(this.status);
    this.attachments = form.querySelector('[data-attachments]') || element('div', { 'data-attachments': '' });
    this.status.after(this.attachments);
    this.attachmentIds = form.querySelector('input[name="attachment_ids"]') || element('input', { type: 'hidden', name: 'attachment_ids' });
    form.append(this.attachmentIds);
    // Only images are allowed (JPG, PNG, WebP, GIF); the server checks again by extension and actual format all the
    // same.
    this.fileInput = element('input', { type: 'file', multiple: '', accept: IMAGE_TYPES, 'data-editor-file': '', 'aria-label': t('js-editor-choose-image') });
    this.fileInput.hidden = true;
    form.append(this.fileInput);
    const fallback = form.querySelector('.upload-plain');
    if (fallback) fallback.hidden = true;
    for (const control of form.querySelectorAll(`button[formaction="${base()}/api/preview"]`)) {
      control.type = 'button';
      this.listen(control, 'click', () => {
        this.preview.hidden = false;
        this.toolbar.querySelector('[data-editor-action="preview"]')?.setAttribute('aria-pressed', 'true');
        this.renderPreview();
      });
    }
    // Set visibility from the server-side category first, so restoring the draft knows which fields were not
    // submitted; the draft may change the category, so set it once more afterwards.
    this.updateActivityVisibility();
    this.restoreDraft();
    this.updateActivityVisibility();
    this.listen(form, 'change', event => {
      if (event.target.name === 'category') this.updateActivityVisibility();
      if (event.target.name === 'entry_mode') this.updateActivityVisibility('[data-click-requirements]');
    });
    enhancePoll(form);
    enhanceDeadline(form);
    this.updateLotteryPrizes = enhanceLotteryPrizes(form, this.abort.signal);
    // The draft may change the selected forum, so after restoring, set the posting guide from the dropdown again.
    this.forumSelect = form.querySelector('select[name="fid"]');
    this.updateGuidance();
    if (this.forumSelect) this.listen(this.forumSelect, 'change', () => this.updateGuidance());
    this.updateAudienceSummary();
    this.listen(form, 'change', event => {
      if (event.target.name === 'fid' || event.target.name === 'visibility') this.updateAudienceSummary();
    });
    for (const node of this.attachments.querySelectorAll('[data-attachment-id]')) this.adoptMedia(node);
    bindPacketPreview(form, this.abort.signal);
    this.listen(this.textarea, 'keydown', event => this.continueMarkup(event));
    this.listen(form, 'compositionstart', () => { this.composing = true; });
    this.listen(form, 'compositionend', () => { this.composing = false; });
    this.listen(this.textarea, 'input', () => {
      const value = this.textarea.value;
      const map = textChange(this.lastBody, value);
      this.mapMarkers(map);
      this.lastBody = value;
      this.changed();
    });
    this.listen(form, 'input', event => {
      if (event.target !== this.textarea) { this.invalidateSubmission(); this.touch(); }
    });
    this.listen(form, 'change', () => this.touch());
    this.listen(this.toolbar, 'mousedown', event => { if (event.target.closest('button')) event.preventDefault(); });
    this.listen(this.toolbar, 'click', event => {
      const action = event.target.closest('[data-editor-action]')?.dataset.editorAction;
      if (action) this.action(action);
    });
    this.listen(this.fileInput, 'change', () => { this.uploadFiles(this.fileInput.files, this.fileMarker); this.fileInput.value = ''; });
    this.listen(this.container, 'dragover', event => {
      if (event.dataTransfer.types.includes('Files')) { event.preventDefault(); this.container.classList.add('is-drop-target'); }
    });
    this.listen(this.container, 'dragleave', () => this.container.classList.remove('is-drop-target'));
    this.listen(this.container, 'drop', event => {
      this.container.classList.remove('is-drop-target');
      if (event.dataTransfer.files.length) { event.preventDefault(); this.uploadFiles(event.dataTransfer.files, this.marker()); }
    });
    this.listen(this.container, 'paste', event => {
      if (event.clipboardData.files.length) { event.preventDefault(); this.uploadFiles(event.clipboardData.files, this.marker()); }
    });
    this.listen(form, 'keydown', event => {
      if (event.key === 'Escape') { this.closePicker(); this.fullscreen(false); }
      if (event.key === 'Enter' && (event.ctrlKey || event.metaKey)) {
        event.preventDefault();
        event.stopPropagation();
        if (!event.isComposing && event.keyCode !== 229 && !this.composing) form.requestSubmit();
      }
    }, { capture: true });
    this.listen(form, 'submit', event => {
      this.saveDraft();
      if (this.originalRequired && !this.textarea.value.trim()) {
        event.preventDefault(); event.stopImmediatePropagation(); this.status.textContent = t('js-editor-body-required'); this.focus();
      }
      // The limit is the admin-area setting settings.posting.body_max_bytes, written by the page on
      // data-body-max-bytes; the server checks again against the value read by the write gate all the same.
      const maxBytes = Number(form.dataset.bodyMaxBytes);
      if (maxBytes > 0 && new TextEncoder().encode(this.textarea.value).length > maxBytes) {
        event.preventDefault(); event.stopImmediatePropagation(); this.status.textContent = t('js-editor-body-too-long', { max: maxBytes }); this.focus();
      }
      // The server takes a post's attachments from its body, and an image only goes into the body once it is checked
      // (`consumeStatus`): posting before that publishes the post without it, for good.
      if (!event.defaultPrevented && this.unsettledMedia().length) {
        event.preventDefault(); event.stopImmediatePropagation(); this.status.textContent = t('js-editor-uploads-unsettled');
      }
      if (!event.defaultPrevented) this.submittedKey = this.form.elements.request_key?.value;
    }, { capture: true });
    // A form reset changes the body without firing an input event: fire one so attachment positions, the draft and
    // the preview keep up.
    this.listen(form, 'reset', () => queueMicrotask(() => this.textarea.dispatchEvent(new Event('input', { bubbles: true }))));
    form.dataset.composerReady = 'true';
  }

  listen(target, type, fn, options = {}) { target.addEventListener(type, fn, { ...options, signal: this.abort.signal }); }
  focus() { this.textarea.focus(); }
  selection() { return { from: this.textarea.selectionStart, to: this.textarea.selectionEnd }; }
  marker() { const marker = { ...this.selection() }; this.markers.add(marker); return marker; }
  mapMarkers(map) { for (const marker of this.markers) { marker.from = map(marker.from); marker.to = map(marker.to); } }
  // The body is always changed through the browser's editing commands so Ctrl+Z can undo it; the browser fires the
  // input event.
  // When the command is unavailable or gives the wrong result, the value is written directly: the body is still
  // correct, only this step does not enter the undo stack.
  edit(text, range) {
    const textarea = this.textarea;
    if (!text && range.from === range.to) return;
    const expected = textarea.value.slice(0, range.from) + text + textarea.value.slice(range.to);
    if (document.activeElement !== textarea) textarea.focus();
    textarea.setSelectionRange(range.from, range.to);
    try { document.execCommand(text ? 'insertText' : 'delete', false, text); } catch { /* Written below from the result. */ }
    if (textarea.value !== expected) {
      textarea.value = expected;
      textarea.dispatchEvent(new Event('input', { bubbles: true }));
    }
  }
  // Enter in lists and quotes continues the next line's markup automatically, and Enter on an empty item ends the list
  // or quote.
  // Enter during IME composition confirms the candidate and is not handled; Shift+Enter breaks the line as usual; so
  // does Enter inside code blocks and HTML blocks.
  continueMarkup(event) {
    if (event.key !== 'Enter' || event.shiftKey || event.altKey || event.ctrlKey || event.metaKey) return;
    if (event.isComposing || event.keyCode === 229 || this.composing) return;
    const { from, to } = this.selection();
    if (from !== to) return;
    const value = this.textarea.value;
    const start = value.lastIndexOf('\n', from - 1) + 1;
    const newline = value.indexOf('\n', from);
    const end = newline < 0 ? value.length : newline;
    const line = value.slice(start, end);
    const [markup, quote, indent = '', bullet, number, delimiter, space = '', task] = LINE_MARKUP.exec(line);
    const listed = Boolean(bullet || number);
    if ((!quote && !listed) || from < start + markup.length || THEMATIC_BREAK.test(line)) return;
    for (let node = markdown.parse(value).resolveInner(from, -1); node; node = node.parent) {
      if (VERBATIM_BLOCKS.has(node.name)) return;
    }
    event.preventDefault();
    if (!line.slice(markup.length).trim()) {
      // Empty item: remove the list marker, keeping the quote if nested in one; with only quote markers, remove them
      // all.
      this.insert('', { from: start + (listed ? quote.length : 0), to: end });
      return;
    }
    const marker = bullet || (number ? `${Number(number) + 1}${delimiter}` : '');
    this.insert(`\n${quote}${indent}${marker}${listed ? space : ''}${task ? '[ ] ' : ''}`, { from, to });
  }

  // The posting guide switches with the selected forum. The server already renders the one for the
  // current selection, so without scripts the selected one is still visible.
  updateGuidance() {
    if (!this.forumSelect) return;
    const fid = this.forumSelect.value;
    for (const node of this.form.querySelectorAll('[data-forum-guidance]')) node.hidden = node.dataset.forumGuidance !== fid;
    const hint = this.form.querySelector('[data-forum-guidance-hint]');
    if (hint) hint.hidden = fid !== '';
  }

  updateAudienceSummary() {
    const summary = this.form.querySelector('[data-audience-summary]');
    if (!summary || !this.forumSelect) return;
    const forum = this.forumSelect.selectedOptions[0];
    summary.hidden = !forum?.value;
    if (summary.hidden) return;
    const choice = this.form.elements.visibility.value;
    let audience = t('js-editor-audience-choose');
    if (choice) {
      audience = choice === 'members' || forum.hasAttribute('data-members-only')
        ? t('js-editor-audience-members') : t('js-editor-audience-public');
    }
    // The option ends with the members-only note the server writes after the forum name (editor-forum-members-only).
    const suffix = t('js-editor-members-only-suffix');
    let forumName = forum.label.replace(/^└ /, '');
    if (forum.hasAttribute('data-members-only') && forumName.endsWith(suffix)) forumName = forumName.slice(0, -suffix.length).trimEnd();
    summary.textContent = t('js-editor-audience-summary', { forum: forumName, audience });
  }

  updateActivityVisibility(selector = '[data-categories]') {
    const category = this.form.elements.category;
    if (!category) return;
    const kind = category.value;
    for (const node of this.form.querySelectorAll(selector)) {
      node.hidden = !node.dataset.categories.split(' ').includes(kind)
        || (node.hasAttribute('data-click-requirements') && this.form.elements.entry_mode.value !== 'click');
      // Hidden category fields keep their draft values but take no part in browser validation or form submission.
      for (const field of node.querySelectorAll('input, select, textarea')) field.disabled = node.hidden;
    }
    this.updateLotteryPrizes?.();
  }

  // range is often the position marker of an attachment or picker, and the input event fired by the editing command
  // moves it along, so copy it first.
  insert(text, { from, to } = this.selection(), selectFrom = text.length, selectTo = selectFrom) {
    this.edit(text, { from, to });
    this.textarea.setSelectionRange(from + selectFrom, from + selectTo);
    this.focus();
  }
  insertUpload(text, marker) {
    // When processing finishes the author may be filling in the title or another field: do not steal focus, and keep
    // the cursor by the preserve rule of setRangeText.
    // While typing in the body, go through the editing command instead so the undo stack is not broken, then put the
    // cursor back by the same rule.
    const textarea = this.textarea;
    const range = { from: marker.from, to: marker.to };
    if (document.activeElement !== textarea) {
      textarea.setRangeText(text, range.from, range.to, 'preserve');
      textarea.dispatchEvent(new Event('input', { bubbles: true }));
      return;
    }
    const { from, to } = this.selection();
    const delta = text.length - (range.to - range.from);
    const map = (position, end) => position > range.to ? position + delta : position > range.from ? range.from + (end ? text.length : 0) : position;
    this.edit(text, range);
    textarea.setSelectionRange(map(from, false), map(to, true));
  }
  replaceBody(value) { this.insert(value, { from: 0, to: this.textarea.value.length }); }
  wrap(before, after, fallback = t('js-editor-text')) {
    const range = this.selection();
    const selected = this.textarea.value.slice(range.from, range.to) || fallback;
    this.insert(before + selected + after, range, before.length, before.length + selected.length);
  }
  prefix(prefix) {
    const range = this.selection();
    range.from = range.from === 0 ? 0 : this.textarea.value.lastIndexOf('\n', range.from - 1) + 1;
    const selected = this.textarea.value.slice(range.from, range.to);
    this.insert(selected.split('\n').map(line => prefix + line).join('\n'), range);
  }
  action(action) {
    const wraps = { bold: ['**', '**'], italic: ['*', '*'], strike: ['~~', '~~'], code: ['\n```\n', '\n```\n', t('js-editor-code-text')], spoiler: ['<spoiler>', '</spoiler>', t('js-editor-spoiler-text')], link: ['[', '](https://)', t('js-editor-link-text')] };
    if (wraps[action]) return this.wrap(...wraps[action]);
    const prefixes = { heading: '## ', quote: '> ', ul: '- ', ol: '1. ' };
    if (prefixes[action]) return this.prefix(prefixes[action]);
    if (action === 'fullscreen') this.fullscreen(!this.form.classList.contains('write-fullscreen'));
    if (action === 'preview') {
      this.preview.hidden = !this.preview.hidden;
      this.toolbar.querySelector('[data-editor-action="preview"]')?.setAttribute('aria-pressed', String(!this.preview.hidden));
      if (!this.preview.hidden) this.renderPreview(); else this.previewAbort?.abort();
    }
    if (action === 'attach') { this.fileMarker = this.marker(); this.fileInput.click(); }
    if (action === 'emoji') this.showEmoji();
    if (action === 'mention') this.showMentions();
  }
  fullscreen(active) {
    this.form.classList.toggle('write-fullscreen', active);
    document.documentElement.classList.toggle('has-write-fullscreen', active);
    this.toolbar.querySelector('[data-editor-action="fullscreen"]')?.setAttribute('aria-pressed', String(active));
  }
  changed() {
    this.invalidateSubmission();
    this.published = false;
    this.previewVersion = (this.previewVersion || 0) + 1;
    this.previewAbort?.abort();
    this.touch();
    if (!this.preview.hidden) { clearTimeout(this.previewTimer); this.previewTimer = setTimeout(() => this.renderPreview(), 350); }
    if (!this.composing) this.completeMention();
  }
  invalidateSubmission() {
    if (this.submittedKey && this.form.elements.request_key?.value === this.submittedKey) {
      this.form.elements.request_key.value = requestKey();
    }
  }
  scheduleSave() { clearTimeout(this.saveTimer); this.saveTimer = setTimeout(() => this.saveDraft(), 400); }
  touch() { this.dirty = true; this.scheduleSave(); }
  saveDraft() {
    this.attachmentIds.value = [...this.media.values()].filter(item => item.state === 'ready' && item.scanStatus === 'clean' && !item.cancelled).map(item => item.aid).join(',');
    if (this.published || this.protectedDraft || !this.dirty) return;
    const attachments = [...this.media.values()].filter(item => item.aid && !item.cancelled).map(item => ({ aid: item.aid, name: item.name, state: item.state, markdown: item.markdown || '', bound: Boolean(item.bound), inserted: Boolean(item.inserted), marker: item.marker ? { ...item.marker } : null }));
    const data = { timestamp: Date.now(), body: this.textarea.value, title: this.form.elements.title?.value || '', fid: this.form.elements.fid?.value || '', attachments, request_key: this.form.elements.request_key?.value || '', expected_post_version: this.form.elements.expected_post_version?.value || '', expected_thread_version: this.form.elements.expected_thread_version?.value || '', reason: this.form.elements.reason?.value || '' };
    if (this.form.elements.visibility) data.visibility = this.form.elements.visibility.value;
    data.activity = {};
    for (const name of ACTIVITY_FIELDS) {
      const field = this.form.elements[name];
      if (field) data.activity[name] = field.type === 'checkbox' && !field.checked ? '' : field.value;
    }
    try { localStorage.setItem(this.draftKey, JSON.stringify(data)); }
    catch { this.status.textContent = t('js-editor-draft-blocked'); }
  }
  restoreDraft() {
    try {
      const data = JSON.parse(localStorage.getItem(this.draftKey));
      if (!data) return;
      if (typeof data.timestamp !== 'number' || Date.now() - data.timestamp > MAX_AGE) { localStorage.removeItem(this.draftKey); return; }
      const editing = Number(this.form.dataset.editPid) > 0;
      if (editing && this.form.dataset.restoreDraft !== 'false' && (data.expected_post_version !== this.form.elements.expected_post_version?.value || data.expected_thread_version !== this.form.elements.expected_thread_version?.value)) {
        this.protectedDraft = true;
        this.status.textContent = t('js-editor-draft-conflict');
        const panel = element('details', { class: 'conflict-note conflict-note--draft', 'data-draft-conflict': '' });
        panel.append(element('summary', {}, t('js-editor-draft-old')));
        const text = element('textarea', { class: 'control', readonly: '', rows: '8', 'aria-label': t('js-editor-draft-old-label') });
        text.value = `${data.title || ''}\n\n${data.body || ''}`;
        const proceed = button(t('js-editor-draft-use-current'), 'accept-current-version');
        this.listen(proceed, 'click', () => { this.protectedDraft = false; this.dirty = true; proceed.disabled = true; this.status.textContent = t('js-editor-draft-editing-current'); this.saveDraft(); });
        panel.append(text, proceed); this.status.after(panel);
        return;
      }
      // A validation response is authoritative; never replace its submitted contents. A quote prefills the body with
      // the quote link; the quote has a draft key of its own, and its draft started from that link too.
      const quoting = Number(this.form.elements.quote_pid?.value) > 0;
      if ((!this.textarea.value || editing || quoting) && this.form.dataset.restoreDraft !== 'false') {
        this.textarea.value = typeof data.body === 'string' ? data.body : '';
        if (this.form.elements.title && typeof data.title === 'string') this.form.elements.title.value = data.title;
        // All new threads share one draft: a forum the page already chose (`/new/<forum>`) outranks the draft's.
        const forum = this.form.elements.fid;
        if (forum instanceof HTMLSelectElement && ![...forum.options].some(option => option.defaultSelected && option.value)
          && typeof data.fid === 'string' && [...forum.options].some(option => option.value === data.fid)) forum.value = data.fid;
        // New-thread drafts from older versions did not save the audience, so on restore the author must pick it
        // again, to avoid publishing it as public.
        if (this.form.elements.visibility) {
          if (['public', 'members'].includes(data.visibility)) this.form.elements.visibility.value = data.visibility;
          else if (!editing) {
            for (const radio of this.form.querySelectorAll('[name="visibility"]')) radio.checked = false;
          }
        }
        for (const name of ACTIVITY_FIELDS) setActivityValue(this.form.elements[name], data.activity?.[name]);
        if (this.form.elements.request_key && typeof data.request_key === 'string' && /^[a-f0-9]{64}$/.test(data.request_key)) this.form.elements.request_key.value = data.request_key;
        if (this.form.elements.reason && typeof data.reason === 'string') this.form.elements.reason.value = data.reason;
        this.lastBody = this.textarea.value;
        this.status.textContent = t('js-editor-draft-restored');
        for (const item of Array.isArray(data.attachments) ? data.attachments : []) {
          if (!/^\d+$/.test(String(item.aid))) continue;
          const length = this.textarea.value.length;
          const from = Number.isInteger(item.marker?.from) ? Math.max(0, Math.min(length, item.marker.from)) : length;
          const to = Number.isInteger(item.marker?.to) ? Math.max(from, Math.min(length, item.marker.to)) : from;
          const marker = { from, to }; this.markers.add(marker);
          const entry = this.mediaEntry({ aid: String(item.aid), name: String(item.name || t('js-editor-attachment')), state: 'processing', bound: Boolean(item.bound), inserted: Boolean(item.inserted), marker });
          this.poll(entry);
        }
      } else if (this.form.dataset.restoreDraft === 'false') {
        // A failed-validation response echoes only the fields it received; category fields that were hidden and hence
        // not submitted still follow the local draft.
        for (const name of ACTIVITY_FIELDS) {
          const field = this.form.elements[name];
          if (field instanceof RadioNodeList ? field[0]?.disabled : field?.disabled) setActivityValue(field, data.activity?.[name]);
        }
      }
    } catch { /* Invalid or unavailable local storage does not block composing. */ }
  }
  publishedSuccessfully() {
    this.published = true;
    clearTimeout(this.saveTimer);
    removeDraft(this.form.dataset.uid, this.form.dataset.draft, this.form.elements.request_key?.value);
    this.replaceBody('');
    this.published = true;
    clearTimeout(this.saveTimer);
    this.media.clear(); this.attachments.replaceChildren(); this.attachmentIds.value = '';
    if (this.form.elements.request_key) this.form.elements.request_key.value = requestKey();
    this.status.textContent = t('js-editor-published');
  }

  async renderPreview() {
    this.previewAbort?.abort();
    const controller = this.previewAbort = new AbortController();
    const version = this.previewVersion;
    const data = new URLSearchParams();
    for (const [key, value] of new FormData(this.form)) if (typeof value === 'string') data.append(key, value);
    if (data.get('fid') === '') data.delete('fid');
    data.set('body', this.textarea.value);
    try {
      const response = await fetch(`${base()}/api/preview`, { method: 'POST', body: data, credentials: 'same-origin', headers: { 'HX-Request': 'true' }, signal: controller.signal });
      const html = await response.text();
      if (!this.form.isConnected || version !== this.previewVersion || controller.signal.aborted) return;
      if (!response.ok) { this.status.textContent = responseMessage(html, t('js-editor-preview-failed')); return; }
      this.preview.innerHTML = html;
      this.toolbar.querySelector('[data-editor-action="preview"]')?.setAttribute('aria-pressed', String(!this.preview.hidden));
    } catch (error) { if (error.name !== 'AbortError') this.status.textContent = t('js-editor-preview-error'); }
  }
  showEmoji() {
    this.closePicker();
    this.picker.replaceChildren(); this.picker.hidden = false;
    const marker = this.pickerMarker = this.marker();
    for (const emoji of ['😀', '😁', '😂', '🥹', '😊', '🤔', '👍', '❤️', '🎉', '🍓', '🙏', '👀']) {
      const control = button(emoji, 'insert-emoji');
      control.addEventListener('click', () => { this.insert(emoji, marker); this.closePicker(); });
      this.picker.append(control);
    }
  }
  completeMention() {
    clearTimeout(this.mentionTimer);
    const { from, to } = this.selection();
    if (from !== to) { if (this.automaticMention) this.closePicker(); return; }
    const match = this.textarea.value.slice(0, from).match(/(?:^|\s)@([^\s@\[\]()]{1,32})$/u);
    if (!match) { if (this.automaticMention) this.closePicker(); return; }
    this.mentionTimer = setTimeout(() => this.showMentions(match[1], { from: from - match[1].length - 1, to: from }, false), 250);
  }
  closePicker() {
    this.mentionAbort?.abort(); clearTimeout(this.mentionTimer);
    this.picker.hidden = true; this.picker.onkeydown = null;
    this.automaticMention = false;
    if (this.pickerMarker) this.markers.delete(this.pickerMarker);
    this.pickerMarker = null;
  }
  showMentions(query = '', range = this.selection(), focus = true) {
    this.closePicker();
    this.automaticMention = !focus;
    const marker = this.pickerMarker = { ...range }; this.markers.add(marker);
    this.picker.replaceChildren(); this.picker.hidden = false;
    const input = element('input', { class: 'control', type: 'search', placeholder: t('js-editor-mention-placeholder'), 'aria-label': t('js-editor-mention-search'), 'data-mention-query': '' });
    input.value = query;
    const results = element('div', { class: 'mention-menu', role: 'listbox', 'aria-label': t('js-editor-mention-results') });
    this.picker.append(input, results);
    const search = async () => {
      this.mentionAbort?.abort();
      if (!input.value.trim()) { results.replaceChildren(); return; }
      const controller = this.mentionAbort = new AbortController();
      try {
        const response = await fetch(`${base()}/api/mentions?q=${encodeURIComponent(input.value)}`, { credentials: 'same-origin', signal: controller.signal });
        if (!response.ok) throw new Error('mentions');
        const doc = new DOMParser().parseFromString(await response.text(), 'text/html');
        if (controller.signal.aborted || !results.isConnected) return;
        results.replaceChildren();
        for (const candidate of doc.querySelectorAll('[data-uid][data-username]')) {
          if (!/^\d+$/.test(candidate.dataset.uid)) continue;
          const control = button(`@${candidate.dataset.username}`, 'insert-mention');
          control.setAttribute('role', 'option');
          control.addEventListener('click', () => {
            const username = candidate.dataset.username.replace(/[\\[\]]/g, '\\$&');
            this.insert(`[@${username}](member:${candidate.dataset.uid}) `, marker);
            this.closePicker();
          });
          results.append(control);
        }
        if (!results.children.length) results.textContent = t('js-editor-mention-none');
      } catch (error) { if (error.name !== 'AbortError') results.textContent = t('js-editor-mention-error'); }
    };
    input.addEventListener('input', () => { clearTimeout(this.mentionTimer); this.mentionTimer = setTimeout(search, 250); });
    this.picker.onkeydown = event => {
      const options = [...results.querySelectorAll('button')];
      const index = options.indexOf(document.activeElement);
      if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
        event.preventDefault(); options[(index + (event.key === 'ArrowDown' ? 1 : options.length - 1)) % options.length]?.focus();
      }
      if (event.key === 'Enter' && document.activeElement === input) { event.preventDefault(); options[0]?.click(); }
    };
    search(); if (focus) input.focus();
  }

  mediaEntry(data) {
    const entry = { key: requestKey(), state: 'uploading', ...data };
    const row = entry.row = element('div', { class: 'write-attachment', 'data-state': entry.state });
    if (entry.aid) row.dataset.attachmentId = entry.aid;
    entry.label = element('span', {}, entry.name);
    entry.message = element('span', { class: 'write-status', role: 'status' }, t('js-editor-upload-waiting'));
    entry.progress = element('progress', { max: '100', value: '0', 'aria-label': t('js-editor-upload-progress', { name: entry.name }) });
    const cancel = button(entry.bound ? t('js-editor-attachment-remove') : t('js-editor-attachment-cancel'), 'cancel-attachment');
    this.listen(cancel, 'click', () => this.cancelMedia(entry));
    entry.retry = button(t('js-editor-retry'), 'retry-attachment'); entry.retry.hidden = true;
    entry.insert = button(t('js-editor-attachment-insert'), 'insert-attachment'); entry.insert.hidden = true;
    this.listen(entry.insert, 'click', () => { if (entry.markdown && entry.state === 'ready' && entry.scanStatus === 'clean') { this.insert(`${entry.markdown}\n`); entry.inserted = true; this.scheduleSave(); } });
    this.listen(entry.retry, 'click', () => { entry.retry.hidden = true; entry.cancelled = false; if (entry.aid) this.poll(entry); else this.upload(entry); });
    row.append(entry.label, entry.progress, entry.message, entry.insert, entry.retry, cancel);
    this.attachments.append(row); this.media.set(entry.key, entry);
    return entry;
  }
  // Images added in this editor that are on their way into the body: not yet uploaded, uploading, or waiting for the
  // check. Failed, cancelled and inserted ones do not hold back posting.
  unsettledMedia() {
    return [...this.media.values()].filter(entry => !entry.cancelled && !entry.inserted
      && (!entry.state || ['uploading', 'pending', 'processing'].includes(entry.state)
        || (entry.state === 'ready' && entry.scanStatus === 'pending')));
  }
  adoptMedia(node) {
    const aid = node.dataset.attachmentId;
    if (!/^\d+$/.test(aid)) return;
    const existing = [...this.media.values()].find(item => item.aid === aid);
    if (existing) { if (node !== existing.row) node.remove(); return; }
    const entry = this.mediaEntry({ aid, name: node.dataset.filename || node.textContent.trim(), state: node.dataset.state, bound: Number(node.dataset.boundPid) > 0, inserted: true });
    node.remove(); this.poll(entry);
  }
  uploadFiles(files, marker = this.marker()) {
    for (const file of files) {
      const entry = this.mediaEntry({ file, name: file.name, marker, key: requestKey() });
      this.dirty = true;
      this.upload(entry);
    }
  }
  upload(entry) {
    if (!entry.file || this.destroyed) return;
    // The server requires the token before the file: it validates the token as soon as it reads it and writes to
    // staging only after that. FormData submits in insertion order.
    const data = new FormData();
    data.set('_csrf', this.form.elements._csrf?.value || '');
    data.set('request_key', entry.key);
    data.set('return_to', sitePath(location.pathname) + location.search);
    data.set('file', entry.file);
    const xhr = entry.xhr = new XMLHttpRequest();
    xhr.open('POST', `${base()}/api/media`); xhr.setRequestHeader('HX-Request', 'true');
    xhr.timeout = 120000;
    entry.state = 'uploading'; entry.row.dataset.state = 'uploading'; entry.progress.hidden = false;
    xhr.upload.onprogress = event => { if (event.lengthComputable) entry.progress.value = event.loaded * 100 / event.total; };
    const failed = message => { if (!entry.cancelled) { entry.state = 'error'; entry.row.dataset.state = 'error'; entry.message.textContent = message; entry.retry.hidden = false; entry.progress.hidden = true; } };
    xhr.onerror = () => failed(t('js-editor-upload-interrupted'));
    xhr.ontimeout = () => failed(t('js-editor-upload-timeout'));
    xhr.onload = () => {
      if (entry.cancelled || this.destroyed) return;
      if (xhr.status < 200 || xhr.status >= 300) return failed(responseMessage(xhr.responseText, t('js-editor-upload-failed', { status: String(xhr.status) })));
      if (!this.consumeStatus(entry, xhr.responseText)) return failed(t('js-editor-upload-unreadable'));
      if (entry.state === 'processing' || entry.state === 'pending') this.poll(entry);
    };
    xhr.send(data);
  }
  consumeStatus(entry, html) {
    const doc = new DOMParser().parseFromString(html, 'text/html');
    const node = doc.querySelector('[data-attachment-id][data-state]');
    if (!node || !/^\d+$/.test(node.dataset.attachmentId)) return false;
    entry.aid = node.dataset.attachmentId; entry.state = node.dataset.state; entry.scanStatus = node.dataset.scanStatus;
    entry.bound = Number(node.dataset.boundPid) > 0;
    const cancel = entry.row.querySelector('[data-editor-action="cancel-attachment"]');
    if (cancel && entry.bound) { cancel.textContent = t('js-editor-attachment-remove'); cancel.setAttribute('aria-label', t('js-editor-attachment-remove')); }
    entry.row.dataset.attachmentId = entry.aid; entry.row.dataset.state = entry.state;
    entry.progress.hidden = true;
    // The server's data-state is always one of the six attachment lifecycles (media::LIFECYCLES).
    entry.message.textContent = node.querySelector('[data-media-message]')?.textContent || ({ ready: t('js-editor-state-ready'), processing: t('js-editor-state-processing'), pending: t('js-editor-state-pending'), rejected: t('js-editor-state-rejected'), purging: t('js-editor-state-purging'), purged: t('js-editor-state-purged') }[entry.state] || t('js-editor-state-working'));
    const detail = node.querySelector('[data-media-detail]')?.textContent.trim();
    if (detail) entry.message.textContent += ` ${detail}`;
    entry.insert.hidden = true;
    if (entry.state === 'ready' && entry.scanStatus === 'clean') {
      const token = node.dataset.insertMarkdown || '';
      // Only insert the private attachment reference emitted for this exact aid.
      if (token.includes(`](attachment:${entry.aid})`) && !/[\r\n]/.test(token)) {
        entry.markdown = token;
        if (!entry.inserted && !this.textarea.value.includes(token)) this.insertUpload(`${token}\n`, entry.marker || this.selection());
        entry.inserted = true;
        entry.insert.hidden = false;
      }
    }
    entry.retry.hidden = !['processing', 'pending'].includes(entry.state);
    this.scheduleSave(); return true;
  }
  // Inspection progress is polled at a gradually slower pace: every 2 seconds for the first 30 seconds, every 5
  // seconds up to 2 minutes, then every 10 seconds, stopping after 5 minutes to wait for a manual retry.
  async poll(entry, started = Date.now()) {
    if (entry.cancelled || this.destroyed) return;
    entry.pollAbort?.abort(); clearTimeout(entry.timer);
    const controller = entry.pollAbort = new AbortController();
    try {
      const pid = Number(this.form.dataset.editPid);
      const response = await fetch(`${base()}/api/media/${entry.aid}${pid > 0 ? `?pid=${pid}` : ''}`, { credentials: 'same-origin', signal: controller.signal });
      const html = await response.text();
      if (this.destroyed || entry.cancelled || controller.signal.aborted || !entry.row.isConnected) return;
      if (!response.ok || !this.consumeStatus(entry, html)) throw new Error('status');
      if (['pending', 'processing'].includes(entry.state)) {
        const elapsed = Date.now() - started;
        const delay = elapsed < 30000 ? 2000 : elapsed < 120000 ? 5000 : 10000;
        if (elapsed + delay <= 300000) entry.timer = setTimeout(() => this.poll(entry, started), delay);
        else { entry.message.textContent = t('js-editor-check-pending'); entry.retry.hidden = false; }
      }
    } catch (error) {
      if (error.name !== 'AbortError') { entry.message.textContent = t('js-editor-check-error'); entry.retry.hidden = false; }
    }
  }
  async cancelMedia(entry) {
    entry.cancelled = true; entry.xhr?.abort(); entry.pollAbort?.abort(); clearTimeout(entry.timer);
    if (entry.aid && !entry.bound) {
      try {
        const data = new URLSearchParams(); data.set('_csrf', this.form.elements._csrf?.value || '');
        const response = await fetch(`${base()}/api/media/${entry.aid}/cancel`, { method: 'POST', body: data, credentials: 'same-origin' });
        if (!response.ok) throw new Error('cancel');
      } catch { if (!this.destroyed) { entry.cancelled = false; entry.message.textContent = t('js-editor-cancel-failed'); } return; }
    }
    if (this.destroyed) return;
    // Remove the exact reference, leaving surrounding prose intact.
    // Parse links/images so titles, reference links and code examples stay correct.
    if (entry.aid) {
      for (const range of attachmentRanges(this.textarea.value, entry.aid)) this.insert('', range);
    }
    this.media.delete(entry.key); entry.row.remove(); this.touch();
  }
  destroy() {
    if (this.destroyed) return;
    this.saveDraft(); this.destroyed = true;
    this.abort.abort(); this.previewAbort?.abort(); this.mentionAbort?.abort();
    clearTimeout(this.saveTimer); clearTimeout(this.previewTimer); clearTimeout(this.mentionTimer);
    for (const entry of this.media.values()) { entry.xhr?.abort(); entry.pollAbort?.abort(); clearTimeout(entry.timer); }
    this.fullscreen(false); forms.delete(this.form);
  }
}

function published(detail) {
  if (!detail || detail.uid == null || typeof detail.draft !== 'string' || typeof detail.request_key !== 'string' || !/^[a-f0-9]{64}$/.test(detail.request_key)) return;
  removeDraft(String(detail.uid), detail.draft, detail.request_key);
  for (const composer of forms.values()) {
    if (composer.form.dataset.uid === String(detail.uid) && composer.form.dataset.draft === detail.draft && composer.form.elements.request_key?.value === detail.request_key) composer.publishedSuccessfully();
  }
}
function mount() {
  const meta = document.querySelector('meta[name="composer-published"]');
  const confirmation = meta?.content;
  if (confirmation) {
    meta.content = '';
    try { published(JSON.parse(confirmation)); } catch { /* Invalid metadata cannot discard a draft. */ }
  }
  for (const form of document.querySelectorAll('form[data-composer]')) if (!forms.has(form)) forms.set(form, new Composer(form));
}
if (!window.gonofComposerInitialized) {
  window.gonofComposerInitialized = true;
  document.addEventListener('DOMContentLoaded', mount);
  document.addEventListener('htmx:afterSwap', mount);
  document.addEventListener('composer:published', event => published(event.detail));
  document.addEventListener('htmx:beforeCleanupElement', event => {
    const removed = event.detail.elt;
    for (const composer of forms.values()) if (removed === composer.form || removed.contains(composer.form)) composer.destroy();
  });
  window.addEventListener('pagehide', () => { for (const composer of forms.values()) composer.saveDraft(); });
  if (document.readyState !== 'loading') mount();
}
