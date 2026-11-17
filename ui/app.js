// This script draws the window of Dupli. Each function draws one part of the window.
(() => {
'use strict';

const TAURI = window.__TAURI__;
const SVG = new Set(['svg', 'path', 'circle']);
const $ = (s, r = document) => r.querySelector(s);

function h(tag, props, ...kids) {
  const e = SVG.has(tag) ? document.createElementNS('http://www.w3.org/2000/svg', tag) : document.createElement(tag);
  for (const [k, v] of Object.entries(props || {})) {
    if (v == null || v === false) continue;
    if (k === 'text') e.textContent = v;
    else if (k.startsWith('on')) e.addEventListener(k.slice(2), v);
    else e.setAttribute(k, v === true ? '' : v);
  }
  for (const kid of kids.flat(3)) {
    if (kid == null || kid === false) continue;
    e.append(kid.nodeType ? kid : document.createTextNode(String(kid)));
  }
  return e;
}

// Draw again only if the data changed.
function draw(node, sig, build) {
  if (node.__sig === sig) return false;
  node.__sig = sig;
  node.replaceChildren(...[build()].flat(3).filter(Boolean));
  return true;
}
function setText(node, text) { if (node.textContent !== text) node.textContent = text; }
function setAttr(node, name, value) {
  if (value == null || value === false) { if (node.hasAttribute(name)) node.removeAttribute(name); }
  else if (node.getAttribute(name) !== String(value)) node.setAttribute(name, value);
}

const nf = new Intl.NumberFormat('en-US');
const num = (x) => nf.format(x);
const count = (x, one, many) => `${num(x)} ${x === 1 ? one : many || one + 's'}`;
const cap = (s) => s.charAt(0).toUpperCase() + s.slice(1);

function size(b) {
  if (b < 1000) return `${b} B`;
  const units = ['KB', 'MB', 'GB', 'TB'];
  let v = b / 1000, i = 0;
  while (v >= 999.5 && i < units.length - 1) { v /= 1000; i++; }
  return `${v >= 100 ? v.toFixed(0) : v.toFixed(1)} ${units[i]}`;
}
const clock = (d) => d.toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' });
function when(ms) {
  if (!ms) return 'never';
  const d = new Date(ms), now = new Date();
  const day = (x) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  const days = Math.round((day(now) - day(d)) / 864e5);
  if (days === 0) return `today, ${clock(d)}`;
  if (days === 1) return `yesterday, ${clock(d)}`;
  if (days > 1 && days < 7) return `${d.toLocaleDateString([], { weekday: 'long' })}, ${clock(d)}`;
  return d.toLocaleDateString([], { day: 'numeric', month: 'short', year: d.getFullYear() === now.getFullYear() ? undefined : 'numeric' });
}
function ago(ms) {
  if (!ms) return 'never';
  const s = (Date.now() - ms) / 1000;
  if (s < 50) return 'just now';
  if (s < 3300) return `${Math.max(1, Math.round(s / 60))} min ago`;
  if (s < 5400) return 'an hour ago';
  return when(ms);
}
function took(ms) {
  const s = ms / 1000;
  if (s < 1) return 'under a second';
  if (s < 90) return `${Math.round(s)} s`;
  if (s < 5400) return `${Math.round(s / 60)} min`;
  return `${(s / 3600).toFixed(1)} h`;
}
function left(s) {
  if (s < 50) return 'less than a minute left';
  if (s < 3300) return `about ${Math.max(1, Math.round(s / 60))} min left`;
  const hrs = Math.floor(s / 3600), min = Math.round((s % 3600) / 60);
  return `about ${hrs} h ${min} min left`;
}
const base = (p) => { const t = p.replace(/[\\/]+$/, ''); const i = Math.max(t.lastIndexOf('/'), t.lastIndexOf('\\')); return t.slice(i + 1) || p; };
const parent = (p) => { const t = p.replace(/[\\/]+$/, ''); const i = Math.max(t.lastIndexOf('/'), t.lastIndexOf('\\')); return i > 0 ? t.slice(0, i) : (i === 0 ? t.slice(0, 1) : ''); };
const sideWord = (side) => (side === 'source' ? 'source' : 'copy');

const S = {
  view: null,
  sel: null,
  tab: 'changes',
  screen: 'pair',
  draft: null,
  folders: {},
  notice: null,
  confirm: null,
  restoring: false,
  shown: true,
};
const cache = {
  groups: { key: '', data: null, open: new Map(), ver: 0 },
  hist: { key: '', data: null, open: new Map(), ver: 0 },
  act: { key: '', data: null, open: new Set(), ver: 0 },
};

let call = () => Promise.reject(new Error('not connected'));
const pairs = () => (S.view ? S.view.pairs : []);
const cur = () => pairs().find((p) => p.config.id === S.sel) || null;
const isReady = (p) => p.online[0] === true && p.online[1] === true;
const isBusy = (p) => p.phase !== 'idle';
const platform = () => (S.view ? S.view.platform : 'macos');
const trayWord = () => (platform() === 'macos' ? 'menu bar' : 'system tray');

function say(text, sticky) {
  S.notice = { text, sticky: !!sticky, at: Date.now() };
  renderNotice();
  if (!sticky) setTimeout(() => { if (S.notice && Date.now() - S.notice.at >= 6900) { S.notice = null; renderNotice(); } }, 7000);
}
const fail = (e) => say(typeof e === 'string' ? e : (e && e.message) || 'That did not work.', true);

const GLYPH = {
  copy: 'M6.5 2.2v8.6M2.2 6.5h8.6',
  update: 'M2.5 4.4h7.4M7.6 2.1l2.3 2.3-2.3 2.3M10.5 8.8H3.1M5.4 6.5 3.1 8.8l2.3 2.3',
  move: 'M1.8 6.5h8.6M7.4 3.4l3.1 3.1-3.1 3.1',
  remove: 'M2.4 6.5h8.2',
  kept: 'M6.5 2.4v3.9l2.6 1.6M11 6.5a4.5 4.5 0 1 1-1.4-3.2',
  alert: 'M6.5 2.4v5.4M6.5 10.2v.4',
  equal: 'M2.4 4.7h8.2M2.4 8.3h8.2',
  unequal: 'M2.4 4.7h8.2M2.4 8.3h8.2M8.7 2.2 4.3 10.8',
  halt: 'M3.6 3.6h5.8v5.8H3.6z',
};
const glyph = (name) => h('svg', { class: 'glyph', viewBox: '0 0 13 13', 'aria-hidden': 'true' }, h('path', { d: GLYPH[name] }));
const twist = () => h('svg', { class: 'twist', viewBox: '0 0 9 9', 'aria-hidden': 'true' }, h('path', { d: 'M2.8 1.2l3.6 3.3-3.6 3.3' }));
const soft = (label, onclick, opts = {}) => h('button', { class: `soft${opts.primary ? ' primary' : ''}${opts.small ? ' small' : ''}`, disabled: opts.disabled, onclick, text: label });
const note = (title, ...paras) => h('div', { class: 'note' }, h('h2', { text: title }), paras);
const para = (...kids) => h('p', null, kids);

function seg(options, value, onpick, label) {
  return h('div', { class: 'seg', role: 'radiogroup', 'aria-label': label },
    options.map(([v, text]) => h('button', { role: 'radio', 'aria-checked': String(v === value), onclick: () => v !== value && onpick(v), text })));
}
const onoff = (value, onpick, label) => seg([[false, 'Off'], [true, 'On']], !!value, onpick, label);
function opt(question, hint, control, cls) {
  return h('div', { class: `opt${cls ? ' ' + cls : ''}` }, h('div', null, h('div', { class: 'q', text: question }), hint && h('div', { class: 'hint' }, hint)), control);
}

function lampFor(p) {
  if (p.phase === 'syncing' || p.phase === 'comparing' || p.phase === 'restoring') return { c: 'green', blink: true };
  switch (p.health) {
    case 'up_to_date': return { c: 'green' };
    case 'changes': return { c: 'amber' };
    case 'held': case 'problem': return { c: 'red' };
    default: return { c: null };
  }
}
function percent(g) {
  if (!g) return 0;
  if (g.bytes_total > 0) return Math.min(100, Math.floor((g.bytes_done / g.bytes_total) * 100));
  return g.files_total > 0 ? Math.min(100, Math.floor((g.files_done / g.files_total) * 100)) : 0;
}
function offlineName(p) {
  const i = p.online[0] === false ? 0 : 1;
  const path = i === 0 ? p.config.source : p.config.copy;
  const ref = i === 0 ? p.config.source_volume : p.config.copy_volume;
  if (ref && ref.kind !== 'internal' && ref.mount) return base(ref.mount);
  return `The ${i === 0 ? 'source' : 'copy'} folder “${base(path)}”`;
}
function jobNote(p) {
  switch (p.phase) {
    case 'queued': return 'Waiting its turn';
    case 'comparing': return 'Comparing…';
    case 'syncing': return `Syncing, ${percent(p.progress)}%`;
    case 'restoring': return 'Restoring…';
  }
  switch (p.health) {
    case 'offline': return 'Not connected';
    case 'up_to_date': return 'Up to date';
    case 'changes': return `${count(changesIn(p.summary), 'change')} waiting`;
    case 'held': return 'Waiting for your go-ahead';
    case 'problem': return 'Needs a look';
    default: return 'Not compared yet';
  }
}
const changesIn = (s) => (s ? (s.copy.n + s.update.n + s.move.n + s.remove.n) || (s.make_dir + s.remove_dir) : 0);
function summaryLine(s) {
  const parts = [];
  if (s.copy.n) parts.push(`${num(s.copy.n)} to copy`);
  if (s.update.n) parts.push(`${num(s.update.n)} to replace`);
  if (s.move.n) parts.push(`${num(s.move.n)} to move`);
  if (s.remove.n) parts.push(`${num(s.remove.n)} to remove`);
  const folders = s.make_dir + s.remove_dir;
  if (!parts.length && folders) parts.push(`${count(folders, 'folder')} to tidy`);
  const bytes = s.copy.bytes + s.update.bytes;
  return parts.join(' · ') + (bytes ? ` · ${size(bytes)} to transfer` : '');
}
function reportLine(r) {
  const parts = [];
  if (r.copied + r.updated) parts.push(`${num(r.copied + r.updated)} copied`);
  if (r.moved) parts.push(`${num(r.moved)} moved`);
  if (r.removed) parts.push(`${num(r.removed)} removed`);
  if (r.touched) parts.push(`${num(r.touched)} re-dated`);
  return parts.length ? parts.join(', ') : 'nothing needed copying';
}
const STEP = { preparing: 'Getting ready', removing: 'Clearing what was removed', moving: 'Moving files into place', copying: 'Copying', finishing: 'Finishing' };

function describe(p) {
  const d = { head: '', sub: '', meter: null };
  if (p.phase === 'comparing' || p.phase === 'queued') {
    d.head = p.phase === 'queued' ? 'Waiting its turn' : 'Comparing';
    d.sub = `Source: ${count(p.scan[0].files, 'file')} read · Copy: ${count(p.scan[1].files, 'file')} read`;
    d.meter = 'sweep';
    return d;
  }
  if (p.phase === 'syncing') {
    const g = p.progress;
    d.head = g ? STEP[g.step] || 'Syncing' : 'Syncing';
    d.meter = percent(g);
    if (g) {
      const bits = [];
      if (g.bytes_total) bits.push(`${size(g.bytes_done)} of ${size(g.bytes_total)}`);
      else if (g.files_total) bits.push(`${num(g.files_done)} of ${count(g.files_total, 'file')}`);
      if (g.rate > 1000) bits.push(`${size(g.rate)}/s`);
      if (g.eta_s != null && g.step === 'copying') bits.push(left(g.eta_s));
      d.sub = [g.current && h('span', { class: 'file', text: g.current }), g.current && bits.length ? ' · ' : '', bits.join(' · ')];
    }
    return d;
  }
  if (p.phase === 'restoring') { d.head = 'Restoring'; d.sub = 'Putting files back in both folders.'; d.meter = 'sweep'; return d; }

  switch (p.health) {
    case 'offline':
      d.head = `${offlineName(p)} isn’t connected`;
      d.sub = p.config.on_connect && !p.config.paused
        ? 'Dupli will sync by itself when it appears.'
        : 'Connect it to compare or sync.';
      break;
    case 'problem':
      if (p.error) { d.head = 'The compare didn’t finish'; d.sub = p.error; }
      else if (p.last && p.last.outcome === 'stopped') { d.head = 'The last sync stopped early'; d.sub = p.last.message || ''; }
      else {
        const r = p.last ? p.last.report : null;
        d.head = 'Synced, with exceptions';
        d.sub = r ? `${count(r.failed + r.skipped, 'file')} could not be handled. The rest is done: ${reportLine(r)}.` : '';
      }
      break;
    case 'held':
      d.head = 'Waiting for your go-ahead';
      d.sub = 'This sync would remove a lot from the copy. Have a look below first.';
      break;
    case 'changes':
      d.head = `${count(changesIn(p.summary), 'change')} waiting`;
      d.sub = summaryLine(p.summary);
      break;
    case 'up_to_date': {
      d.head = 'Up to date';
      const st = p.stats ? `${count(p.stats[0].files, 'file')}, ${size(p.stats[0].bytes)} · ` : '';
      d.sub = `${st}checked ${ago(p.checked_ms)}`;
      break;
    }
    default:
      d.head = p.online.includes(null) ? 'Looking for the folders' : 'Not compared yet';
      d.sub = p.online.includes(null) ? '' : 'Compare reads both folders and lists what a sync would do. Nothing changes until you press Sync.';
  }
  return d;
}

function renderRail() {
  const paused = S.view.settings.auto_paused;
  const sw = $('#auto-switch');
  setAttr(sw, 'data-pos', paused ? 0 : 1);
  sw.querySelectorAll('[role=radio]').forEach((b) => setAttr(b, 'aria-checked', String((b.dataset.pos === '0') === paused)));
  setAttr($('#auto-lamp'), 'data-c', paused ? 'amber' : 'green');
  setAttr($('#auto-lamp'), 'title', paused ? 'Automatic syncing is paused' : 'Automatic syncing is on');
  setAttr($('#setup-key'), 'aria-pressed', String(S.screen === 'setup'));
  setAttr($('#new-pair'), 'aria-pressed', String(S.screen === 'new'));
}

function renderBank() {
  const box = $('#pairs');
  const list = pairs();
  const order = list.map((p) => p.config.id).join('|') || 'none';
  draw(box, order, () => list.length
    ? list.map((p) => h('div', { class: 'job', role: 'option', 'data-id': p.config.id },
        h('span', { class: 'lamp' }),
        h('button', { class: 'window', onclick: () => select(p.config.id) },
          h('span', { class: 'lcd' }, h('span', { class: 'job-name' }), h('span', { class: 'job-note' })))))
    : h('p', { class: 'bank-empty', text: 'No pairs yet. A pair is a folder and the copy you keep of it.' }));
  for (const p of list) {
    const row = box.querySelector(`[data-id="${CSS.escape(p.config.id)}"]`);
    if (!row) continue;
    const lamp = lampFor(p), note = jobNote(p);
    setAttr(row, 'aria-selected', String(S.screen === 'pair' && p.config.id === S.sel));
    setAttr(row.firstChild, 'data-c', lamp.c);
    setAttr(row.firstChild, 'data-blink', lamp.blink ? '' : null);
    setAttr(row.firstChild, 'title', note);
    setText(row.querySelector('.job-name'), p.config.name);
    setText(row.querySelector('.job-note'), note);
  }
}

function select(id) {
  S.sel = id;
  S.screen = 'pair';
  S.confirm = null;
  render();
}

function plateData() {
  if (S.screen === 'new' && S.draft) {
    const d = S.draft;
    const side = (path) => {
      const f = path ? S.folders[path] : null;
      return { path, online: f ? f.reachable : null, volume: f ? f.volume : null };
    };
    return { sides: [side(d.source), side(d.copy)], mode: d.mode, moving: false, pair: null, editable: true };
  }
  const p = cur();
  if (!p) return { sides: [{}, {}], mode: null, moving: false, pair: null, editable: false };
  return {
    sides: [0, 1].map((i) => ({ path: i ? p.config.copy : p.config.source, online: p.online[i], volume: p.volumes[i], ref: i ? p.config.copy_volume : p.config.source_volume })),
    mode: p.config.mode, moving: p.phase === 'syncing', pair: p, editable: S.screen === 'pair' && !isBusy(p),
  };
}
function volumeLine(s) {
  if (!s.path) return '';
  if (s.online === false) return 'Not connected';
  const v = s.volume;
  if (!v) return '';
  const where = v.kind === 'network' && v.server ? `${v.name} on ${v.server}` : v.name;
  return `${where} · ${size(v.free)} free`;
}
function renderPath() {
  const data = plateData();
  const plates = document.querySelectorAll('.plate');
  data.sides.forEach((s, i) => {
    const el = plates[i];
    setAttr(el, 'data-off', String(!s.path || s.online === false));
    setAttr(el.querySelector('.lamp'), 'data-c', s.path && s.online === true ? 'green' : null);
    setText(el.querySelector('.plate-vol'), volumeLine(s));
    setText(el.querySelector('.plate-name'), s.path ? base(s.path) : (i ? 'No copy folder' : 'No source folder'));
    draw(el.querySelector('.plate-path'), s.path || '', () => {
      if (!s.path) return [S.screen === 'new' ? 'Choose one below' : ' '];
      const dir = parent(s.path);
      const sep = s.path.includes('\\') ? '\\' : '/';
      return [h('span', { class: 'lead', text: dir }), h('span', { class: 'tail', text: (dir.endsWith(sep) || !dir ? '' : sep) + base(s.path) })];
    });
    const win = el.querySelector('.window');
    win.disabled = !data.pair || s.online !== true;
    setAttr(win, 'title', data.pair && s.online === true ? `Open ${s.path}` : null);
  });
  const sw = $('#mode-switch');
  const pos = { mirror: 0, add_only: 1, two_way: 2 }[data.mode];
  setAttr(sw, 'data-pos', pos == null ? null : pos);
  setAttr(sw, 'aria-disabled', String(!data.editable));
  sw.querySelectorAll('[role=radio]').forEach((b) => setAttr(b, 'aria-checked', String(Number(b.dataset.pos) === pos)));
  const live = data.sides.every((s) => s.path && s.online !== false);
  setAttr($('.flow'), 'data-moving', String(data.moving));
  setAttr($('.chevrons.fwd'), 'data-lit', String(live));
  setAttr($('.chevrons.back'), 'data-lit', String(live));
  setAttr($('.chevrons.back'), 'data-off', String(data.mode !== 'two_way'));
}

const MODES = ['mirror', 'add_only', 'two_way'];
const MODE_SAYS = {
  mirror: 'The copy is made identical to the source. Anything in the copy that is not in the source is taken out.',
  add_only: 'New and changed files are copied across. Nothing is ever removed from the copy.',
  two_way: 'Both folders are kept the same. A change or deletion on either side is carried to the other.',
};
function pickMode(mode) {
  if (S.screen === 'new' && S.draft) { S.draft.mode = mode; render(); return; }
  const p = cur();
  if (!p || p.config.mode === mode || isBusy(p)) return;
  savePair({ mode });
}

function renderHead() {
  let d;
  let aside = null;
  if (S.screen === 'new') d = { head: 'New pair', sub: 'Choose a folder and where its copy lives. Nothing is copied until you press Sync.' };
  else if (S.screen === 'setup') d = { head: 'Setup', sub: 'How Dupli itself behaves, for every pair.' };
  else {
    const p = cur();
    if (!p) d = { head: 'No pairs yet', sub: 'A pair is two folders: a source, and the copy Dupli keeps matching it.' };
    else {
      d = describe(p);
      aside = [h('small', { text: 'Last in sync' }), h('strong', { text: p.synced_ms ? cap(when(p.synced_ms)) : 'Never' })];
    }
  }
  setText($('#headline'), d.head);
  const sub = $('#sub');
  if (typeof d.sub === 'string') { sub.__sig = null; setText(sub, d.sub); }
  else draw(sub, JSON.stringify(d.sub.map((x) => (x && x.nodeType ? x.textContent : x))), () => d.sub);
  draw($('#aside'), aside ? aside.map((n) => n.textContent).join('|') : '', () => aside || []);
  const meter = $('#meter');
  meter.hidden = d.meter == null;
  setAttr(meter, 'data-sweep', String(d.meter === 'sweep'));
  meter.firstChild.style.width = typeof d.meter === 'number' ? `${d.meter}%` : '';
}

const TABS = [['changes', 'Changes'], ['history', 'History'], ['activity', 'Activity'], ['options', 'Options']];
function renderTabs() {
  const p = S.screen === 'pair' ? cur() : null;
  const nav = $('#tabs');
  nav.hidden = !p;
  if (!p) return;
  const counts = { changes: p.health === 'changes' || p.health === 'held' ? changesIn(p.summary) : 0, history: p.history ? p.history.files : 0 };
  draw(nav, `${S.tab}|${counts.changes}|${counts.history}`, () => TABS.map(([id, label]) =>
    h('button', { class: 'tab', role: 'tab', 'aria-selected': String(S.tab === id), onclick: () => { S.tab = id; S.confirm = null; render(); } },
      label, counts[id] ? h('span', { class: 'n', text: num(counts[id]) }) : null)));
}

function renderNotice() {
  const bar = $('#notice');
  bar.hidden = !S.notice;
  if (S.notice) draw(bar, S.notice.text + S.notice.at, () => [h('span', { text: S.notice.text }), soft('Dismiss', () => { S.notice = null; renderNotice(); }, { small: true })]);
}

function renderBody() {
  const body = $('#body');
  let sig, build;
  if (S.screen === 'new') { sig = 'new|' + JSON.stringify(S.draft) + JSON.stringify(S.folders); build = newPairView; }
  else if (S.screen === 'setup') { sig = 'setup|' + JSON.stringify(S.view.settings) + S.confirm; build = setupView; }
  else {
    const p = cur();
    if (!p) { sig = 'welcome'; build = welcomeView; }
    else if (S.tab === 'changes') {
      refreshGroups(p);
      sig = ['c', p.config.id, p.phase, p.health, p.checked_ms, JSON.stringify(p.summary), JSON.stringify(p.guard), p.error, p.last && p.last.at_ms,
        p.online.join(), cache.groups.ver, p.config.keep_history, p.config.history_days, p.config.guard_percent, p.config.on_connect].join('|');
      build = () => changesView(p);
    } else if (S.tab === 'history') {
      refreshHistory(p);
      sig = ['h', p.config.id, p.online.join(), cache.hist.ver, S.confirm, S.restoring, p.phase !== 'idle', p.config.keep_history, p.config.history_days].join('|');
      build = () => historyView(p);
    } else if (S.tab === 'activity') {
      refreshActivity(p);
      sig = ['a', p.config.id, cache.act.ver].join('|');
      build = () => activityView(p);
    } else {
      if (body.contains(document.activeElement) && document.activeElement.matches('input, textarea') && body.__sig && body.__sig.startsWith('o|' + p.config.id)) return;
      sig = ['o', p.config.id, JSON.stringify(p.config), S.confirm, p.volumes[1] && p.volumes[1].kind, platform()].join('|');
      build = () => optionsView(p);
    }
  }
  const keep = body.scrollTop;
  const same = (body.__sig || '').split('|').slice(0, 2).join('|') === sig.split('|').slice(0, 2).join('|');
  if (draw(body, sig, build)) body.scrollTop = same ? keep : 0;
}

function refreshGroups(p) {
  const want = p.summary && changesIn(p.summary) + p.notes > 0 ? `${p.config.id}|${p.checked_ms}` : '';
  const g = cache.groups;
  if (g.key === want) return;
  g.key = want;
  g.data = null;
  g.open = new Map();
  g.ver++;
  if (!want) return;
  call('plan_groups', { id: p.config.id }).then((data) => {
    if (g.key !== want) return;
    g.data = data;
    g.ver++;
    renderBody();
  }).catch(fail);
}

const OPS = [
  ['copy', 'To copy'],
  ['update', 'To replace'],
  ['move', 'To move or rename'],
  ['remove', 'To remove'],
];
const groupKey = (g) => [g.op, g.side, g.dir, g.from_dir].join('\u0001');
const inDir = (dir, name) => (dir ? [h('span', { class: 'dim', text: dir + '/' }), h('b', { text: name })] : h('b', { text: name }));

function groupRows(p, g) {
  const two = p.config.mode === 'two_way';
  const chips = [
    two && g.side === 'source' ? h('span', { class: 'chip', text: 'into source' }) : null,
    g.conflicts ? h('span', { class: 'chip solid', title: 'Both sides changed. The newer one wins; the other is kept in History.', text: g.conflicts === g.count ? 'both changed' : `${g.conflicts} both changed` }) : null,
  ];
  const sizeCell = g.op === 'move' ? '' : size(g.bytes);
  if (g.count === 1) {
    const what = g.op === 'move'
      ? [inDir(g.from_dir, g.sample_from), h('span', { class: 'arrow', text: '→' }), g.from_dir === g.dir ? h('b', { text: g.sample }) : inDir(g.dir, g.sample)]
      : inDir(g.dir, g.sample);
    return [h('div', { class: 'row' }, glyph(g.op), h('span', { class: 'what' }, what, chips), h('span', { class: 'num' }), h('span', { class: 'num size', text: sizeCell }))];
  }
  const key = groupKey(g);
  const open = cache.groups.open.get(key);
  const label = g.op === 'move'
    ? (g.from_dir === g.dir
        ? [h('b', { text: g.dir || 'Top folder' }), h('span', { class: 'dim', text: ' · renamed' })]
        : [h('b', { text: g.from_dir || 'Top folder' }), h('span', { class: 'arrow', text: '→' }), h('b', { text: g.dir || 'Top folder' })])
    : h('b', { text: g.dir || 'Top folder' });
  const rows = [h('button', { class: 'row', 'aria-expanded': String(!!open), onclick: () => toggleGroup(p, g) },
    twist(), h('span', { class: 'what' }, label, chips), h('span', { class: 'num', text: count(g.count, 'file') }), h('span', { class: 'num size', text: sizeCell }))];
  if (open) {
    for (const f of open.files) {
      rows.push(h('div', { class: 'row sub-row' }, glyph(g.op),
        h('span', { class: 'what' }, f.from_name && f.from_name !== f.name ? [h('span', { class: 'dim', text: f.from_name }), h('span', { class: 'arrow', text: '→' })] : null, f.name,
          f.conflict ? h('span', { class: 'chip solid', text: 'both changed' }) : null),
        h('span', { class: 'num' }), h('span', { class: 'num size', text: g.op === 'move' ? '' : size(f.size) })));
    }
    if (open.files.length < g.count) {
      rows.push(h('div', { class: 'row sub-row' }, h('span'), h('span', { class: 'what dim', text: `and ${count(g.count - open.files.length, 'more file')}` }),
        h('span', { class: 'num' }), soft('Show more', () => loadGroup(p, g, open.files.length), { small: true })));
    }
  }
  return rows;
}
function toggleGroup(p, g) {
  const key = groupKey(g), open = cache.groups.open;
  if (open.has(key)) { open.delete(key); cache.groups.ver++; renderBody(); }
  else loadGroup(p, g, 0);
}
function loadGroup(p, g, offset) {
  const key = groupKey(g), want = cache.groups.key;
  call('plan_files', { id: p.config.id, group: { op: g.op, side: g.side, dir: g.dir, from_dir: g.from_dir }, offset, limit: 200 }).then((files) => {
    if (cache.groups.key !== want) return;
    const had = offset ? (cache.groups.open.get(key) || { files: [] }).files : [];
    cache.groups.open.set(key, { files: had.concat(files) });
    cache.groups.ver++;
    renderBody();
  }).catch(fail);
}

function fateOf(p) {
  if (!p.config.keep_history) return 'deleted for good (History is off)';
  return p.config.history_days ? `kept in History for ${count(p.config.history_days, 'day')}` : 'kept in History';
}
function guardCallout(p) {
  const g = p.guard, keeps = p.config.keep_history;
  const after = keeps
    ? `They would be ${fateOf(p)}, not deleted.`
    : 'History is off for this pair, so they would be deleted for good.';
  let title, text;
  if (g.kind === 'empty_side') {
    title = `The ${sideWord(g.side)} folder is empty`;
    text = `Syncing would take all ${count(g.other_files, 'file')} out of the ${sideWord(g.side === 'source' ? 'copy' : 'source')}. That is rarely meant: check that this is the right folder and that its drive is really connected. ${after}`;
  } else {
    title = `This would remove ${num(g.removals)} of ${count(g.total, 'file')} from the ${sideWord(g.side)}`;
    text = `That is more than the ${p.config.guard_percent}% this pair checks with you about. ${after}`;
  }
  return h('div', { class: 'callout' }, h('h2', { text: title }), h('p', { text }),
    h('div', { class: 'actions' }, soft(g.kind === 'empty_side' ? 'Remove them and sync' : 'Sync anyway', () => call('sync', { id: p.config.id, confirmed: true }).catch(fail), { primary: true })));
}
const NOTE_SAYS = {
  link: (n) => `${count(n, 'link')} left alone. Dupli copies real files, not shortcuts to them.`,
  unreadable: (n) => `${count(n, 'folder')} could not be read, so nothing in ${n === 1 ? 'it' : 'them'} was touched on either side.`,
  name_clash: (n) => `${count(n, 'name')} differ only by capital letters or accents from another in the same folder. The other drive cannot hold both, so the second was skipped.`,
  newer_in_copy: (n) => `${count(n, 'file')} newer in the copy than in the source ${n === 1 ? 'was' : 'were'} kept as ${n === 1 ? 'it is' : 'they are'}.`,
  type_clash: (n) => `${count(n, 'name')} is a file on one side and a folder on the other, and was left alone.`,
};
function notesView(data) {
  if (!data || !data.notes_total) return null;
  const kinds = new Map();
  for (const n of data.notes) { if (!kinds.has(n.kind)) kinds.set(n.kind, []); kinds.get(n.kind).push(n.rel); }
  return h('div', { class: 'fine' }, [...kinds].map(([kind, rels]) => h('div', null, NOTE_SAYS[kind] ? NOTE_SAYS[kind](rels.length) : '',
    h('ul', null, rels.slice(0, 6).map((r) => h('li', { text: r })), rels.length > 6 ? h('li', { text: `and ${num(rels.length - 6)} more` }) : null))));
}

function changesView(p) {
  const id = p.config.id;
  if (p.phase === 'comparing' || p.phase === 'queued') {
    return note('Reading both folders', para('Names, sizes and dates are compared, and files that look moved are checked by their contents. Nothing is changed by this.'));
  }
  if (p.phase === 'restoring') return note('Putting files back', para('Each file goes back into the copy and into the source, so the two agree.'));
  if (p.health === 'offline' && p.phase === 'idle') {
    return note(`${offlineName(p)} isn’t connected`,
      para(p.synced_ms ? ['The two folders last matched ', h('b', { text: when(p.synced_ms) }), '.'] : 'These two folders have not been synced yet.'),
      para(p.config.on_connect && !p.config.paused && !S.view.settings.auto_paused
        ? 'When it appears, Dupli will compare and sync by itself.'
        : 'Connect it, then press Compare.'));
  }
  const data = cache.groups.data;
  const out = [];
  if (p.phase === 'idle' && p.health === 'problem') {
    if (p.error) return note('The compare didn’t finish', para(p.error), h('div', { class: 'actions' }, soft('Try again', () => call('compare', { id }).catch(fail), { primary: true })));
    const r = p.last ? p.last.report : null;
    out.push(note(p.last && p.last.outcome === 'stopped' ? 'The last sync stopped early' : 'Some files were left out',
      para(p.last && p.last.message ? p.last.message : `${count(r ? r.failed + r.skipped : 0, 'file')} could not be handled in the last sync. Compare again to see what is still waiting.`),
      h('div', { class: 'actions' }, soft('Compare again', () => call('compare', { id }).catch(fail), { primary: true }))));
    if (r && r.problems.length) out.push(problemsView(r.problems));
    return out;
  }
  if (p.phase === 'idle' && p.health === 'unknown') {
    return note('Not compared yet', para('Compare reads both folders and lists exactly what a sync would copy, move and remove. Nothing changes until you press Sync.'));
  }
  if (p.phase === 'idle' && p.health === 'up_to_date') {
    const st = p.stats;
    out.push(note('Nothing to do', para(st ? `Both folders hold the same ${count(st[0].files, 'file')} (${size(st[0].bytes)}).` : 'Both folders match.')));
    out.push(notesView(data));
    return out;
  }
  if (p.guard && p.phase === 'idle') out.push(guardCallout(p));
  if (!data) { out.push(h('div', { class: 'fine', text: p.summary ? 'Listing the changes…' : '' })); return out; }
  for (const [op, title] of OPS) {
    const groups = data.groups.filter((g) => g.op === op);
    if (!groups.length) continue;
    const s = p.summary ? p.summary[op] : null;
    out.push(h('div', { class: 'sect' }, title,
      s ? h('span', { class: 'tally', text: `${count(s.n, 'file')}${op === 'move' ? '' : ', ' + size(s.bytes)}` }) : null,
      op === 'remove' || op === 'update' ? h('span', { class: 'fate', text: `${op === 'update' ? 'old versions ' : ''}${fateOf(p)}` }) : null,
      op === 'move' ? h('span', { class: 'fate', text: 'renamed in place, nothing copied' }) : null));
    for (const g of groups) out.push(groupRows(p, g));
  }
  if (data.more) out.push(h('div', { class: 'fine', text: `and ${count(data.more, 'more file')} in other folders.` }));
  if (p.summary && !data.groups.length && p.summary.make_dir + p.summary.remove_dir > 0) {
    out.push(note('Only folders to tidy', para(`${count(p.summary.make_dir, 'empty folder')} to add and ${num(p.summary.remove_dir)} to remove. No files are affected.`)));
  }
  out.push(notesView(data));
  return out;
}
function problemsView(problems) {
  return [h('div', { class: 'sect' }, 'What was left out', h('span', { class: 'tally', text: count(problems.length, 'file') })),
    problems.map((x) => h('div', { class: 'row' }, glyph('alert'), h('span', { class: 'what' }, h('b', { text: x.rel }), h('span', { class: 'dim', text: ` · ${x.what}` })), h('span', { class: 'num' }), h('span', { class: 'num' })))];
}

function refreshHistory(p) {
  const hc = cache.hist;
  const reachable = p.online[1] === true && (p.config.mode !== 'two_way' || p.online[0] === true);
  const want = reachable ? `${p.config.id}|${p.last ? p.last.at_ms : 0}|${p.history ? p.history.files : '-'}|${p.phase === 'idle'}` : '';
  if (hc.key === want) return;
  const sameRuns = hc.key.split('|')[0] === want.split('|')[0];
  hc.key = want;
  if (!sameRuns) { hc.data = null; hc.open = new Map(); }
  hc.ver++;
  if (!want) return;
  call('history', { id: p.config.id }).then((data) => {
    if (hc.key !== want) return;
    hc.data = data;
    for (const k of [...hc.open.keys()]) if (!data.runs.some((r) => r.side + '|' + r.id === k)) hc.open.delete(k);
    hc.ver++;
    renderBody();
    for (const k of hc.open.keys()) { const [side, run] = k.split('|'); loadRun(p, side, run); }
  }).catch(fail);
}
function loadRun(p, side, run) {
  const k = side + '|' + run, want = cache.hist.key;
  call('history_entries', { id: p.config.id, side, run }).then((entries) => {
    if (cache.hist.key !== want) return;
    cache.hist.open.set(k, entries);
    cache.hist.ver++;
    renderBody();
  }).catch(fail);
}
function restore(p, side, run, rels) {
  S.restoring = true;
  renderBody();
  call('history_restore', { id: p.config.id, side, run, rels }).then((r) => {
    const renamed = r.restored.filter((x) => x.placed !== x.rel);
    let text = r.restored.length ? `${count(r.restored.length, 'file')} put back in both folders.` : '';
    if (renamed.length) text += ` “${base(renamed[0].rel)}” went back as “${base(renamed[0].placed)}”, because a newer file has its name.`;
    if (r.failed.length) text += ` ${count(r.failed.length, 'file')} could not be restored: ${r.failed[0][1]}`;
    say(text.trim() || 'Nothing was restored.', r.failed.length > 0);
  }).catch(fail).finally(() => { S.restoring = false; cache.hist.key = '!'; render(); });
}
function historyView(p) {
  const id = p.config.id;
  const keeps = p.config.keep_history;
  const lasts = p.config.history_days ? ` for ${count(p.config.history_days, 'day')}` : ' until you clear it';
  if (!cache.hist.key) {
    return note('History is on the drive', para(`What Dupli takes out of the copy is kept on the copy’s own drive. Connect ${offlineName(p)} to see it.`));
  }
  const H = cache.hist.data;
  if (!H) return h('div', { class: 'fine', text: 'Reading what has been kept…' });
  if (!H.runs.length) {
    return note('Nothing kept yet', para(keeps
      ? `When a sync removes a file from the copy, or replaces it with a newer version, the old one is moved here${lasts}. Restore puts it back in both folders.`
      : 'History is off for this pair: removed and replaced files are deleted straight away. You can turn it on under Options.'));
  }
  const two = p.config.mode === 'two_way';
  const busy = S.restoring || p.phase !== 'idle';
  const out = [];
  out.push(h('div', { class: 'strip' },
    h('div', { class: 'say' }, h('b', { text: count(H.usage.files, 'file') }), ` kept, ${size(H.usage.bytes)}. `, keeps ? `Each stays${lasts}.` : 'History is now off; these remain until cleared.'),
    S.confirm === 'clear'
      ? [h('span', { text: 'Delete them for good?' }), soft('Delete', () => { S.confirm = null; call('history_clear', { id }).then((g) => { say(`${count(g.files, 'kept file')} deleted, ${size(g.bytes)} freed.`); cache.hist.key = '!'; render(); }).catch(fail); }, { primary: true }), soft('Keep', () => { S.confirm = null; renderBody(); })]
      : [soft('Show folder', () => call('history_reveal', { id, side: 'copy', run: null, rel: null }).catch(fail)), soft('Empty history…', () => { S.confirm = 'clear'; renderBody(); }, { disabled: busy })]));
  for (const run of H.runs) {
    const k = run.side + '|' + run.id;
    const entries = cache.hist.open.get(k);
    out.push(h('button', { class: 'row', 'aria-expanded': String(!!entries), onclick: () => { if (entries) { cache.hist.open.delete(k); cache.hist.ver++; renderBody(); } else loadRun(p, run.side, run.id); } },
      twist(), h('span', { class: 'what' }, h('b', { text: cap(when(run.time_ms)) }), two ? h('span', { class: 'chip', text: `from the ${sideWord(run.side)}` }) : null),
      h('span', { class: 'num', text: count(run.count, 'file') }), h('span', { class: 'num size', text: size(run.bytes) })));
    if (!entries) continue;
    out.push(h('div', { class: 'row sub-row' }, h('span'), h('span', { class: 'what dim', text: 'Taken out by this sync' }),
      soft('Show', () => call('history_reveal', { id, side: run.side, run: run.id, rel: null }).catch(fail), { small: true }),
      soft(`Restore all ${num(entries.length)}`, () => restore(p, run.side, run.id, entries.map((e) => e.rel)), { small: true, disabled: busy })));
    for (const e of entries.slice(0, 400)) {
      out.push(h('div', { class: 'row sub-row' }, glyph(e.reason === 'replaced' ? 'update' : 'remove'),
        h('span', { class: 'what' }, inDir(parent(e.rel), base(e.rel)), h('span', { class: 'dim', text: e.reason === 'replaced' ? ' · older version' : ' · removed' })),
        h('span', { class: 'num size', text: size(e.size) }),
        soft('Restore', () => restore(p, run.side, run.id, [e.rel]), { small: true, disabled: busy })));
    }
    if (entries.length > 400) out.push(h('div', { class: 'row sub-row' }, h('span'), h('span', { class: 'what dim', text: `and ${count(entries.length - 400, 'more file')}. “Show” opens the folder with all of them.` }), h('span'), h('span')));
  }
  return out;
}

function refreshActivity(p) {
  const a = cache.act;
  const want = `${p.config.id}|${p.last ? p.last.at_ms : 0}`;
  if (a.key === want) return;
  if (a.key.split('|')[0] !== p.config.id) { a.data = null; a.open = new Set(); }
  a.key = want;
  a.ver++;
  call('activity', { id: p.config.id }).then((data) => { if (a.key !== want) return; a.data = data; a.ver++; renderBody(); }).catch(fail);
}
const TRIGGER = { manual: 'by hand', startup: 'when Dupli started', connect: 'drive connected', change: 'files changed', timer: 'on the timer' };
function activityView(p) {
  const list = cache.act.data;
  if (!list) return h('div', { class: 'fine', text: 'Reading the log…' });
  if (!list.length) return note('No syncs yet', para('Every sync that changes something is listed here: when it ran, what set it off, and what it did.'));
  const out = [];
  for (const r of list) {
    const rep = r.report;
    let what;
    switch (r.outcome) {
      case 'synced': what = cap(reportLine(rep)); break;
      case 'partial': what = `${cap(reportLine(rep))} · ${count(rep.failed + rep.skipped, 'file')} left out`; break;
      case 'cancelled': what = `Stopped by you · ${reportLine(rep)}`; break;
      case 'stopped': what = `Stopped early · ${r.message || ''}`; break;
      case 'held': what = 'Waited for your go-ahead'; break;
      default: what = r.message || 'Failed';
    }
    const detail = [rep.bytes ? size(rep.bytes) : null, rep.elapsed_ms ? took(rep.elapsed_ms) : null].filter(Boolean).join(' in ');
    const has = rep.problems && rep.problems.length > 0;
    const key = String(r.at_ms);
    const open = cache.act.open.has(key);
    const mark = { synced: 'equal', partial: 'unequal', held: 'alert' }[r.outcome] || 'halt';
    const cells = [has ? twist() : glyph(mark),
      h('span', { class: 'what' }, h('b', { text: cap(when(r.at_ms)) }), h('span', { class: 'dim', text: ` · ${TRIGGER[r.trigger] || ''} · ` }), what),
      h('span', { class: 'num' }), h('span', { class: 'num', text: detail })];
    out.push(has
      ? h('button', { class: 'row', 'aria-expanded': String(open), onclick: () => { open ? cache.act.open.delete(key) : cache.act.open.add(key); cache.act.ver++; renderBody(); } }, cells)
      : h('div', { class: 'row' }, cells));
    if (has && open) {
      for (const x of rep.problems) out.push(h('div', { class: 'row sub-row' }, glyph('alert'), h('span', { class: 'what' }, x.rel, h('span', { class: 'dim', text: ` · ${x.what}` })), h('span'), h('span')));
    }
  }
  return out;
}

function savePair(patch) {
  const p = cur();
  if (!p) return Promise.resolve();
  return call('save_pair', { pair: { ...p.config, ...patch } }).catch(fail);
}
function folderRow(label, path, info, onpick) {
  const v = info && info.volume;
  const detail = !path ? '' : info && info.reachable === false ? 'Not reachable right now'
    : [path, v ? `${v.name}${v.kind === 'network' && v.server ? ' on ' + v.server : ''}` : null, info && info.items != null ? (info.items === 0 ? 'empty' : info.items >= 1000 ? '1,000+ items' : count(info.items, 'item')) : null].filter(Boolean).join(' · ');
  return h('div', { class: 'opt' }, h('div', { class: 'folder' },
    h('div', { class: `where${path ? '' : ' empty'}` },
      h('b', null, h('span', { class: `chip${label === 'Copy' ? ' solid' : ''}`, text: label }), path ? base(path) : 'Not chosen yet'),
      h('span', { text: detail || '\u00a0' })),
    h('span')), soft(path ? 'Change…' : 'Choose…', onpick, { primary: !path }));
}
function chooseFolder(title, start) {
  return call('choose_folder', { title, start: start || null }).then((info) => { if (info) S.folders[info.path] = info; return info; });
}
const KEEP = [[0, 'Don’t keep'], [7, '7 days'], [30, '30 days'], [90, '90 days'], [365, '1 year'], [-1, 'Until cleared']];
const LIMIT = [[0, 'No limit'], [5, '5 GB'], [20, '20 GB'], [50, '50 GB'], [100, '100 GB']];
const TIMER = [[0, 'Off'], [15, '15 min'], [60, 'Hourly'], [360, '6 hours'], [1440, 'Daily']];
const GUARD = [[10, '10%'], [30, '30%'], [50, '50%'], [0, 'Never ask']];

function optionsView(p) {
  const c = p.config;
  const keepValue = !c.keep_history ? 0 : c.history_days === 0 ? -1 : c.history_days;
  const external = p.volumes[1] ? p.volumes[1].kind !== 'internal' : (c.copy_volume && c.copy_volume.kind !== 'internal');
  const text = (value, key, placeholder) => h('input', { class: 'field', value, placeholder, spellcheck: 'false',
    onchange: (e) => savePair({ [key]: e.target.value }), onkeydown: (e) => { if (e.key === 'Enter') e.target.blur(); } });
  const skip = h('textarea', { class: 'field', spellcheck: 'false', placeholder: '*.tmp\nLightroom Previews.lrdata\n/Exports/Cache',
    onchange: (e) => savePair({ excludes: e.target.value.split('\n').map((s) => s.trim()).filter(Boolean) }) });
  skip.value = c.excludes.join('\n');
  const repick = (side) => () => chooseFolder(`Choose the ${side} folder for “${c.name}”`, side === 'source' ? c.source : c.copy)
    .then((info) => info && savePair({ [side]: info.path })).catch(fail);

  return [
    h('div', { class: 'group-title', text: 'This pair' }),
    opt('Name', null, text(c.name, 'name', base(c.source))),
    folderRow('Source', c.source, { reachable: p.online[0] !== false, volume: p.volumes[0] }, repick('source')),
    folderRow('Copy', c.copy, { reachable: p.online[1] !== false, volume: p.volumes[1] }, repick('copy')),
    opt('How it syncs', MODE_SAYS[c.mode], seg([['mirror', 'Mirror'], ['add_only', 'Add only'], ['two_way', 'Two-way']], c.mode, (mode) => savePair({ mode }), 'How it syncs'), 'tall'),

    h('div', { class: 'group-title', text: 'Sync by itself' }),
    opt('When the drive is connected', 'As soon as both folders are reachable: a drive plugged in, a share coming online, or Dupli starting up.', onoff(c.on_connect, (v) => savePair({ on_connect: v }), 'When the drive is connected')),
    opt('When files change', p.config.mode === 'two_way'
      ? 'A few seconds after changes stop, on either side. Changes made over the network by another computer are only seen by the timer.'
      : 'A few seconds after changes in the source stop. Changes made over the network by another computer are only seen by the timer.',
      onoff(c.on_change, (v) => savePair({ on_change: v }), 'When files change')),
    opt('On a timer', null, seg(TIMER, c.every_minutes, (v) => savePair({ every_minutes: v }), 'On a timer')),
    opt('Pause this pair', 'Leaves it out of automatic syncing. Compare and Sync still work by hand.', onoff(c.paused, (v) => savePair({ paused: v }), 'Pause this pair')),
    external && platform() !== 'windows' ? opt('Eject the drive afterwards', 'After a sync set off by plugging the drive in, so it is safe to unplug when the notification arrives.', onoff(c.eject_after, (v) => savePair({ eject_after: v }), 'Eject the drive afterwards')) : null,

    h('div', { class: 'group-title', text: 'History' }),
    opt('Keep removed and replaced files', 'They are moved to a .dupli/history folder inside the copy instead of being deleted, and can be restored from the History tab.',
      seg(KEEP, keepValue, (v) => savePair(v === 0 ? { keep_history: false } : { keep_history: true, history_days: v === -1 ? 0 : v }), 'Keep removed and replaced files'), 'tall'),
    c.keep_history ? opt('Let history take up at most', 'When it grows past this, the oldest runs are deleted first.', seg(LIMIT, c.history_max_gb, (v) => savePair({ history_max_gb: v }), 'History size limit')) : null,
    opt('Ask before removing more than', 'A sync that would take out more than this share of the copy waits for your go-ahead. An empty source always asks.',
      seg(GUARD, c.guard_percent, (v) => savePair({ guard_percent: v }), 'Ask before removing more than')),

    h('div', { class: 'group-title', text: 'What to compare' }),
    h('div', { class: 'opt stack' }, h('div', null, h('div', { class: 'q', text: 'Skip' }),
      h('div', { class: 'hint', text: 'One pattern per line. A name matches anywhere; a pattern with a slash is a path from the top. System clutter such as .DS_Store and Thumbs.db is always skipped.' })), skip),
    opt('Skip hidden files', 'Names that start with a dot.', onoff(c.skip_hidden, (v) => savePair({ skip_hidden: v }), 'Skip hidden files')),
    opt('Follow moved and renamed files', 'A file that only changed place is renamed in the copy instead of being copied again.', onoff(c.detect_moves, (v) => savePair({ detect_moves: v }), 'Follow moved and renamed files')),
    opt('Check contents when only the date differs', 'Reads both files before deciding to copy. Slower to compare, but saves recopying files that some other tool re-dated.', onoff(c.check_contents, (v) => savePair({ check_contents: v }), 'Check contents when only the date differs')),
    opt('Read every copy back', 'After writing a file, read it from the drive and compare it with the original. About twice as slow.', onoff(c.verify, (v) => savePair({ verify: v }), 'Read every copy back')),

    h('div', { class: 'form-foot' },
      S.confirm === 'remove'
        ? [h('span', { class: 'say' }, `Remove “${c.name}” from Dupli? Both folders, and anything kept in History, stay exactly as they are.`),
           soft('Remove', () => { S.confirm = null; call('remove_pair', { id: c.id }).catch(fail); }, { primary: true }), soft('Keep', () => { S.confirm = null; renderBody(); })]
        : [h('span', { class: 'say', text: 'Changes here are saved as you make them.' }), soft('Remove this pair…', () => { S.confirm = 'remove'; renderBody(); })]),
  ];
}

function startNew() {
  call('blank_pair').then((blank) => {
    S.draft = blank;
    S.screen = 'new';
    S.confirm = null;
    render();
  }).catch(fail);
}
function newPairView() {
  const d = S.draft;
  const pick = (side) => () => chooseFolder(side === 'source' ? 'Choose the folder to keep a copy of' : 'Choose where the copy lives', d[side])
    .then((info) => { if (info) { d[side] = info.path; if (side === 'source' && !d.name) d.name = ''; render(); } }).catch(fail);
  const copyInfo = d.copy ? S.folders[d.copy] : null;
  const name = h('input', { class: 'field', placeholder: d.source ? base(d.source) : 'Named after the source folder', spellcheck: 'false', oninput: (e) => { d.name = e.target.value; } });
  name.value = d.name;
  const ready = d.source && d.copy;
  return [
    folderRow('Source', d.source, d.source ? S.folders[d.source] : null, pick('source')),
    folderRow('Copy', d.copy, copyInfo, pick('copy')),
    copyInfo && copyInfo.items > 0 && d.mode === 'mirror'
      ? h('div', { class: 'warn', text: 'The copy folder already has files in it. Anything there that is not in the source will be moved to History on the first sync. You will see the full list, and can change your mind, before anything happens.' }) : null,
    opt('Name', null, name),
    opt('How it syncs', MODE_SAYS[d.mode], seg([['mirror', 'Mirror'], ['add_only', 'Add only'], ['two_way', 'Two-way']], d.mode, pickMode, 'How it syncs'), 'tall'),
    opt('Sync when the drive is connected', null, onoff(d.on_connect, (v) => { d.on_connect = v; render(); }, 'Sync when the drive is connected')),
    opt('Sync when files change', null, onoff(d.on_change, (v) => { d.on_change = v; render(); }, 'Sync when files change')),
    h('div', { class: 'form-foot' },
      h('span', { class: 'say', text: ready ? 'Dupli will compare the two folders and show you what a first sync would do.' : 'Choose both folders to go on.' }),
      soft('Cancel', () => { S.screen = 'pair'; S.draft = null; render(); }),
      soft('Create pair', () => call('save_pair', { pair: d }).then((id) => { S.draft = null; S.sel = id; S.screen = 'pair'; S.tab = 'changes'; render(); }).catch(fail), { primary: true, disabled: !ready })),
  ];
}
function welcomeView() {
  return note('Set up your first pair',
    para('Pick a folder, and pick where its copy should live: an external drive, a memory stick, a share on a NAS.'),
    para('Dupli compares the two and copies only what differs. If you rename or reorganize folders, it renames them in the copy instead of copying everything again. Anything it takes out of the copy is kept in a history folder, so a mistake can be undone.'),
    h('div', { class: 'actions' }, soft('New pair', startNew, { primary: true })));
}

function setupView() {
  const s = S.view.settings;
  const set = (patch) => call('set_settings', { settings: { ...s, ...patch } }).catch(fail);
  return [
    opt('Start Dupli when you log in', `It starts out of the way, in the ${trayWord()}.`, onoff(s.launch_at_login, (v) => set({ launch_at_login: v }), 'Start Dupli when you log in')),
    opt('Keep running when the window is closed', `Dupli stays in the ${trayWord()} and goes on syncing. With this off, closing the window quits.`, onoff(s.keep_in_tray, (v) => set({ keep_in_tray: v }), 'Keep running when the window is closed')),
    opt('Notifications', 'When a sync is waiting for you, when one fails, and when a drive you plugged in is up to date.', onoff(s.notifications, (v) => set({ notifications: v }), 'Notifications')),
    opt('Sounds', 'Keys click, a relay pulls in when a sync starts, and a bell rings when it is done. Automatic syncs are only heard while this window is in view, and in the ' + trayWord() + ' Dupli is silent.', onoff(s.sounds !== false, (v) => { set({ sounds: v }); if (v) { Sound.set(true); Sound.play('done', false); } }, 'Sounds')),
    h('div', { class: 'form-foot' },
      h('span', { class: 'say', text: `Dupli ${S.view.version}. Set in Barlow, used under the SIL Open Font License.` }),
      soft('Done', () => { S.screen = 'pair'; render(); }),
      soft('Quit Dupli', () => call('quit').catch(fail))),
  ];
}

function renderKeys() {
  const p = S.screen === 'pair' ? cur() : null;
  const idle = p && p.phase === 'idle' && isReady(p);
  $('#key-compare').disabled = !idle;
  $('#key-sync').disabled = !idle;
  $('#key-stop').disabled = !(p && isBusy(p));
  for (const [id, key] of [['#key-connect', 'on_connect'], ['#key-change', 'on_change']]) {
    const k = $(id);
    k.disabled = !p;
    setAttr(k, 'aria-pressed', String(!!(p && p.config[key] && !p.config.paused)));
    setAttr(k, 'title', p && p.config.paused ? 'This pair is paused (see Options)' : null);
  }
}

function render() {
  if (!S.view) return;
  if (S.screen === 'pair' && (!S.sel || !cur())) S.sel = pairs().length ? pairs()[0].config.id : null;
  renderRail();
  renderBank();
  renderPath();
  renderHead();
  renderTabs();
  renderBody();
  renderNotice();
  renderKeys();
}

const Sound = window.DupliSound || { set() {}, play() {}, wake() {} };
const inView = () => S.shown && document.visibilityState === 'visible';
const cue = (name, ...args) => { if (S.shown) Sound.play(name, ...args); };
const keyWeight = (k) => (k.classList.contains('key-sync') ? 1 : k.classList.contains('key-tiny') ? 0.12
  : k.classList.contains('key-small') || k.classList.contains('key-latch') ? 0.3 : 0.55);

function wireSound() {
  let down = null;
  const press = (k) => { down = k; cue('keyDown', keyWeight(k)); };
  const release = () => { if (down) { cue('keyUp', keyWeight(down)); down = null; } };
  document.addEventListener('pointerdown', (e) => { Sound.wake(); const k = e.target.closest('.key'); if (k && !k.disabled) press(k); }, true);
  window.addEventListener('pointerup', release, true);
  window.addEventListener('pointercancel', release, true);
  document.addEventListener('keydown', (e) => {
    Sound.wake();
    if ((e.key === ' ' || e.key === 'Enter') && !e.repeat) {
      const k = document.activeElement && document.activeElement.closest('.key');
      if (k && !k.disabled) press(k);
    }
  }, true);
  document.addEventListener('keyup', (e) => { if (e.key === ' ' || e.key === 'Enter') release(); }, true);
  document.addEventListener('click', (e) => {
    const g = e.target.closest('.soft, .tab, button.row, .seg button, .job .window, .plate .window');
    if (g && !g.disabled) cue('soft', g.matches('.window') ? 1 : 0);
  }, true);
}

const RANK = { fault: 6, attention: 5, done: 4, start: 3, queue: 2, ready: 2, connect: 1, disconnect: 1 };
// Make one sound for each change in the data.
function hear(prev, next) {
  if (!prev) return;
  let best = null;
  let stepped = false;
  const seen = inView();
  for (const p of next.pairs) {
    const o = prev.pairs.find((x) => x.config.id === p.config.id);
    if (!o) continue;
    const was = o.phase, is = p.phase;
    const byHand = (p.trigger || o.trigger) === 'manual';
    const want = (name, arg) => { if ((seen || byHand) && (!best || RANK[name] > RANK[best[0]])) best = [name, arg]; };
    if (was !== is) {
      if (was === 'idle' && (is === 'queued' || is === 'comparing')) want('queue');
      else if (is === 'syncing') want('start');
      else if (is === 'idle' && was === 'syncing') {
        if (p.health === 'up_to_date') want('done', true);
        else if (p.last && p.last.outcome === 'cancelled') want('disconnect');
        else want('fault');
      } else if (is === 'idle' && (was === 'comparing' || was === 'queued')) {
        if (p.health === 'held') want('attention');
        else if (p.health === 'problem') want('fault');
        else if (p.health === 'changes') want('ready');
        else if (p.health === 'up_to_date') want('done', false);
      } else if (is === 'idle' && was === 'restoring') want('done', false);
    }
    for (const i of [0, 1]) {
      if (seen && o.online[i] === false && p.online[i] === true) want('connect');
      if (seen && o.online[i] === true && p.online[i] === false) want('disconnect');
    }
    if (p.config.id === S.sel && is === 'syncing' && p.progress && o.progress
      && Math.floor(percent(p.progress) / 2) > Math.floor(percent(o.progress) / 2)) stepped = true;
  }
  if (!S.shown) return;
  if (best) Sound.play(best[0], best[1]);
  else if (stepped && seen && document.hasFocus()) Sound.play('tick');
}

function wire() {
  wireSound();
  $('#new-pair').addEventListener('click', () => { cue('latch', S.screen !== 'new'); if (S.screen === 'new') { S.screen = 'pair'; S.draft = null; render(); } else startNew(); });
  $('#setup-key').addEventListener('click', () => { cue('latch', S.screen !== 'setup'); S.screen = S.screen === 'setup' ? 'pair' : 'setup'; S.confirm = null; render(); });
  $('#auto-switch').addEventListener('click', (e) => {
    const b = e.target.closest('[data-pos]');
    const want = b ? b.dataset.pos === '0' : !S.view.settings.auto_paused;
    if (want === S.view.settings.auto_paused) return;
    cue('slide', 1);
    call('set_settings', { settings: { ...S.view.settings, auto_paused: want } }).catch(fail);
  });
  $('#mode-switch').addEventListener('click', (e) => {
    const sw = e.currentTarget;
    if (sw.getAttribute('aria-disabled') === 'true') return;
    const b = e.target.closest('[data-pos]');
    let pos = b ? Number(b.dataset.pos) : null;
    if (pos == null) { const r = sw.querySelector('.slide-track').getBoundingClientRect(); pos = Math.max(0, Math.min(2, Math.floor(((e.clientX - r.left) / r.width) * 3))); }
    const from = Number(sw.dataset.pos);
    if (pos !== from) cue('slide', Math.abs(pos - from) || 1);
    pickMode(MODES[pos]);
  });
  document.querySelectorAll('.plate .window').forEach((w, i) => w.addEventListener('click', () => {
    const p = cur();
    if (p && S.screen === 'pair') call('open_folder', { id: p.config.id, side: i ? 'copy' : 'source' }).catch(fail);
  }));
  $('#key-compare').addEventListener('click', () => { const p = cur(); if (p) { S.tab = 'changes'; call('compare', { id: p.config.id }).catch(fail); render(); } });
  $('#key-sync').addEventListener('click', () => { const p = cur(); if (p) { S.tab = 'changes'; call('sync', { id: p.config.id, confirmed: false }).catch(fail); render(); } });
  $('#key-stop').addEventListener('click', () => { const p = cur(); if (p) call('stop', { id: p.config.id }).catch(fail); });
  $('#key-connect').addEventListener('click', () => { const p = cur(); if (!p) return; const on = p.config.paused || !p.config.on_connect; cue('latch', on); savePair(p.config.paused ? { paused: false, on_connect: true } : { on_connect: on }); });
  $('#key-change').addEventListener('click', () => { const p = cur(); if (!p) return; const on = p.config.paused || !p.config.on_change; cue('latch', on); savePair(p.config.paused ? { paused: false, on_change: true } : { on_change: on }); });
  $('#win-min').addEventListener('click', () => call('window_minimize').catch(() => {}));
  $('#win-close').addEventListener('click', () => call('window_close').catch(() => {}));
  setInterval(() => { if (S.view && S.screen === 'pair') renderHead(); }, 30000);
  window.addEventListener('keydown', (e) => {
    if (e.key === 'Escape' && S.screen !== 'pair') { S.screen = 'pair'; S.draft = null; render(); }
  });
}

function boot(bridge) {
  call = bridge.invoke;
  wire();
  bridge.listen('state', (view) => { Sound.set(view.settings.sounds !== false); hear(S.view, view); S.view = view; render(); });
  bridge.listen('window-visible', (shown) => { S.shown = !!shown; });
  bridge.listen('select', (id) => select(id));
  bridge.listen('tauri://drag-drop', (drop) => {
    const path = drop && drop.paths && drop.paths[0];
    if (!path) return;
    const place = (info) => {
      if (!info.reachable) return;
      S.folders[info.path] = info;
      if (S.screen !== 'new' || !S.draft) return startNewWith(info.path);
      if (!S.draft.source) S.draft.source = info.path; else S.draft.copy = info.path;
      render();
    };
    call('describe_folder', { path }).then(place).catch(fail);
  });
  call('state').then((view) => {
    S.view = view;
    Sound.set(view.settings.sounds !== false);
    document.body.dataset.platform = view.platform;
    render();
    requestAnimationFrame(() => requestAnimationFrame(() => call('ui_ready').then((shown) => { S.shown = shown !== false; }).catch(() => {})));
  }).catch(fail);
}
function startNewWith(source) {
  call('blank_pair').then((blank) => { S.draft = { ...blank, source }; S.screen = 'new'; render(); }).catch(fail);
}

window.addEventListener('error', (e) => { if (TAURI) TAURI.core.invoke('ui_log', { text: `${e.message} @ ${e.filename}:${e.lineno}` }).catch(() => {}); });

boot({ invoke: (cmd, args) => TAURI.core.invoke(cmd, args), listen: (name, fn) => TAURI.event.listen(name, (e) => fn(e.payload)) });
})();
