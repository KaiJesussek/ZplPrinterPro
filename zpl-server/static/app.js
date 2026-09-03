'use strict';

// ZPL Printer single-page UI (Task 22). Vanilla JS, no build step.
// Talks to the Task 21 API:
//   GET  /api/labels            list, newest first
//   GET  /api/labels/{id}       {meta, ir, diags}   | .zpl | .png
//   PUT  /api/printer-state     partial patch, echoes merged Conditions
//   GET  /api/storage           images/fonts/formats with sizes
//   GET  /api/diagnostics       boot counters
//   SSE  /api/events            event: label (LabelRow) | state (Conditions)

// ---------- helpers ----------
const $ = (sel) => document.querySelector(sel);

async function getJSON(url) {
  const r = await fetch(url);
  if (!r.ok) throw new Error(`${url}: HTTP ${r.status}`);
  return r.json();
}

function esc(s) {
  return String(s).replace(/[&<>"']/g, (c) =>
    ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
}

function fmtBytes(b) {
  if (b >= 1 << 20) return (b / 1048576).toFixed(1) + ' MiB';
  if (b >= 1024) return (b / 1024).toFixed(1) + ' KiB';
  return b + ' B';
}

// ---------- printer-state panel ----------
// The exact `Conditions` fields the API serves (state.rs); unknown options
// 400 on PUT, so the option lists mirror the serde enum variants.
const CONDITIONS = [
  { key: 'paper_out', label: 'Paper out', type: 'bool' },
  { key: 'ribbon_out', label: 'Ribbon out', type: 'bool' },
  { key: 'head_open', label: 'Head open', type: 'bool' },
  { key: 'rewind_full', label: 'Rewinder full', type: 'bool' },
  { key: 'head_temp', label: 'Head temp', type: 'enum', options: ['Normal', 'Warm', 'OverTemp'] },
  { key: 'pause', label: 'Pause', type: 'bool' },
  { key: 'comm_diag', label: 'Comm diag', type: 'bool' },
  { key: 'media_type', label: 'Media', type: 'enum', options: ['DieCut', 'Continuous'] },
  { key: 'print_mode', label: 'Print mode', type: 'enum', options: ['DirectThermal', 'ThermalTransfer'] },
  { key: 'static_ram', label: 'Static RAM', type: 'bool' },
];

const stateInputs = {};

// Last server-confirmed Conditions (boot GET, SSE `state`, PUT echo),
// merged so partial patches never shrink it. The boot GET returns the full
// set, so every control has a snapshot value after the first paint.
// ponytail: one merged snapshot, no per-key provenance - reverts repaint
// every control from it, which only works because the boot GET seeds all keys.
let stateSnapshot = {};

const stateStatus = document.createElement('div');
stateStatus.id = 'state-status';
stateStatus.hidden = true;

function buildStatePanel() {
  const panel = $('#state-panel');
  panel.prepend(stateStatus);
  for (const c of CONDITIONS) {
    const label = document.createElement('label');
    label.className = 'cond';
    const span = document.createElement('span');
    span.textContent = c.label;
    let input;
    if (c.type === 'bool') {
      input = document.createElement('input');
      input.type = 'checkbox';
    } else {
      input = document.createElement('select');
      for (const o of c.options) {
        const opt = document.createElement('option');
        opt.value = o;
        opt.textContent = o;
        input.appendChild(opt);
      }
    }
    input.dataset.key = c.key;
    input.addEventListener('change', () => {
      const body = {};
      body[c.key] = input.type === 'checkbox' ? input.checked : input.value;
      fetch('/api/printer-state', {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      })
        .then((r) => (r.ok ? r.json() : Promise.reject(new Error(`PUT ${c.key}: HTTP ${r.status}`))))
        // Success: the echoed merged state is authoritative (snapshot + paint).
        .then(setStateValues)
        .catch((e) => {
          console.error(e);
          // The input already flipped optimistically; the server rejected the
          // PUT, so repaint every control from the last confirmed snapshot.
          paintState(stateSnapshot);
          setStateStatus('Condition update failed - reverted');
        });
    });
    stateInputs[c.key] = input;
    label.append(span, input);
    panel.appendChild(label);
  }
}

// Repaint controls from a full or partial Conditions object (no snapshot or
// status side effects) - the revert path for optimistic input. Direct value
// assignment never fires `change`, so this is also safe from the SSE
// `state` handler (no echo-PUT loop).
function paintState(cond) {
  for (const [key, input] of Object.entries(stateInputs)) {
    if (key in cond) {
      if (input.type === 'checkbox') input.checked = !!cond[key];
      else input.value = cond[key];
    }
  }
}

// Apply authoritative server state: merge into the snapshot, repaint, and
// clear any stale failure line.
function setStateValues(cond) {
  Object.assign(stateSnapshot, cond);
  setStateStatus('');
  paintState(cond);
}

function setStateStatus(msg) {
  stateStatus.textContent = msg;
  stateStatus.hidden = !msg;
}

// ---------- diagnostics badge ----------
async function loadDiagnostics() {
  const d = await getJSON('/api/diagnostics');
  const n = d.warnings + d.unknown_commands;
  const badge = $('#diag-badge');
  badge.textContent = n ? `${n} issue${n === 1 ? '' : 's'}` : 'no issues';
  badge.title = Object.entries(d).map(([k, v]) => `${k}: ${v}`).join('\n');
  badge.classList.toggle('warn', n > 0);
}

// ---------- storage table ----------
async function loadStorage() {
  const s = await getJSON('/api/storage');
  $('summary').textContent =
    `Storage (${s.counts.images} image${s.counts.images === 1 ? '' : 's'}, ` +
    `${s.counts.fonts} font${s.counts.fonts === 1 ? '' : 's'}, ` +
    `${s.counts.formats} format${s.counts.formats === 1 ? '' : 's'})`;
  const tbody = $('#storage-table tbody');
  tbody.textContent = '';
  const rows = [
    ...Object.entries(s.images).map(([name, im]) => ['image', name, `${im.width}\u00d7${im.height} px`, im.bytes]),
    ...Object.entries(s.fonts).map(([name, bytes]) => ['font', name, '', bytes]),
    ...Object.entries(s.formats).map(([name, bytes]) => ['format', name, '', bytes]),
  ];
  if (!rows.length) {
    tbody.innerHTML = '<tr><td colspan="4" class="hint">Nothing stored.</td></tr>';
    return;
  }
  for (const [kind, name, det, bytes] of rows) {
    const tr = document.createElement('tr');
    tr.innerHTML = `<td>${kind}</td><td>${esc(name)}</td><td>${esc(det)}</td><td>${fmtBytes(bytes)}</td>`;
    tbody.appendChild(tr);
  }
}

// ---------- label list ----------
const labelList = $('#label-list');

function makeRow(row) {
  const card = document.createElement('button');
  card.type = 'button';
  card.className = 'label-row';
  card.title = `#${row.id} \u2014 ${row.width}\u00d7${row.height} dots, ${fmtBytes(row.bytes)}`;
  card.innerHTML =
    `<img src="/api/labels/${row.id}.png" loading="lazy" alt="label ${row.id}">` +
    `<div class="meta">` +
    `<span class="lid">#${row.id}</span>` +
    `<span class="ts">${new Date(row.ts_ms).toLocaleString()}</span>` +
    (row.diags ? `<span class="badge">${row.diags} diag</span>` : '') +
    `</div>`;
  card.addEventListener('click', () => openDetail(row.id));
  return card;
}

async function loadList() {
  const rows = await getJSON('/api/labels');
  labelList.textContent = '';
  if (!rows.length) {
    labelList.innerHTML = '<p class="hint">No labels yet &mdash; send one to the TCP port.</p>';
    return;
  }
  for (const row of rows) labelList.appendChild(makeRow(row)); // rows are newest-first
}

function prependRow(row) {
  if (labelList.querySelector('.hint')) labelList.textContent = '';
  labelList.prepend(makeRow(row));
}

// ---------- detail pane ----------
const detailPane = $('#detail-pane');
const detailImg = $('#detail-img');
const overlay = $('#overlay');
const overlayOn = $('#overlay-on');

let currentIR = null;

// Raw ZPL is the exact bytes received over TCP (any code page), so decode
// with strict UTF-8 first and fall back to windows-1252, whose 1:1 byte map
// renders every byte (e.g. CP437/CP850 text) without `.text()`'s U+FFFD.
async function fetchZplText(r) {
  if (!r.ok) return `(HTTP ${r.status})`;
  const bytes = new Uint8Array(await r.arrayBuffer());
  try {
    return new TextDecoder('utf-8', { fatal: true }).decode(bytes);
  } catch {
    return new TextDecoder('windows-1252').decode(bytes);
  }
}

async function openDetail(id) {
  const [d, zpl] = await Promise.all([
    getJSON(`/api/labels/${id}`),
    fetch(`/api/labels/${id}.zpl`).then(fetchZplText),
  ]);
  detailPane.hidden = false;
  $('#detail-id').textContent = `#${id}`;
  currentIR = d.ir;
  $('#ir-view').textContent = JSON.stringify(d.ir, null, 2);
  $('#zpl-view').textContent = zpl;
  const diagList = $('#diag-list');
  diagList.textContent = '';
  for (const m of d.diags || []) {
    const p = document.createElement('div');
    p.className = 'diag';
    p.textContent = m;
    diagList.appendChild(p);
  }
  overlayOn.checked = false;
  detailImg.src = `/api/labels/${id}.png`;
}

// Field overlay: draw a bounding box per IR field on a canvas over the
// rendered PNG. The bitmap is drawn at 1 dot = 1 px, so a uniform scale
// from the IR label width maps every field's dot coords onto the image.
// Box extents are per-kind: boxes/circles/diagonals/ellipses are exact,
// text/barcodes/graphics use their real params with a coarse glyph-width
// estimate - the inspection tool, not a geometry reimplementation.
const FIELD_COLORS = {
  Text: '#c62828', Barcode: '#1565c0', Box: '#2e7d32', Circle: '#6a1b9a',
  Diagonal: '#ef6c00', Ellipse: '#00838f', Graphic: '#4e342e',
};

function fieldBox(f) {
  const kind = Object.keys(f)[0];
  const v = f[kind];
  switch (kind) {
    case 'Text': {
      const h = v.font_height || 9;
      const w = (v.block && v.block.width) || v.data.length * (v.font_width || Math.round(h * 0.6));
      return { x: v.x, y: v.y, w: Math.max(w, 1), h, label: kind };
    }
    case 'Barcode':
      return { x: v.x, y: v.y, w: Math.max(2, v.data.length * v.module * 2), h: Math.max(v.height, 1), label: kind };
    case 'Box':
      return { x: v.x, y: v.y, w: Math.max(v.width, 1), h: Math.max(v.height, 1), label: kind };
    case 'Circle':
      return { x: v.x, y: v.y, w: Math.max(v.diameter, 1), h: Math.max(v.diameter, 1), label: kind };
    case 'Diagonal':
    case 'Ellipse':
      return { x: v.x, y: v.y, w: Math.max(v.width, 1), h: Math.max(v.height, 1), label: kind };
    case 'Graphic': {
      const w = Math.max(v.bytes_per_row, 1);
      return { x: v.x, y: v.y, w, h: Math.max(Math.ceil(v.data.length / w), 1), label: kind };
    }
    default:
      return null;
  }
}

function drawOverlay() {
  const ctx = overlay.getContext('2d');
  const img = detailImg;
  overlay.width = img.naturalWidth || 1;
  overlay.height = img.naturalHeight || 1;
  ctx.clearRect(0, 0, overlay.width, overlay.height);
  if (!overlayOn.checked || !currentIR || !img.naturalWidth) return;
  const s = img.naturalWidth / Math.max(currentIR.width_dots, 1);
  for (const f of currentIR.fields || []) {
    const b = fieldBox(f);
    if (!b) continue;
    const color = FIELD_COLORS[b.label] || '#000';
    const x = b.x * s, y = b.y * s, w = b.w * s, h = b.h * s;
    ctx.fillStyle = color + '30';
    ctx.strokeStyle = color;
    ctx.lineWidth = Math.max(1, s);
    ctx.fillRect(x, y, w, h);
    ctx.strokeRect(x, y, w, h);
    ctx.fillStyle = color;
    ctx.font = `${Math.max(10, 12 * s)}px monospace`;
    ctx.fillText(b.label, x + 2, y + Math.max(10, 12 * s));
  }
}

detailImg.addEventListener('load', drawOverlay);
overlayOn.addEventListener('change', drawOverlay);

// ---------- live updates (SSE) ----------
const es = new EventSource('/api/events');
// The server ends the stream when a subscriber lags (the browser then
// reconnects automatically); repaint the list and panels from GETs on
// every (re)connect so nothing is lost while the stream was stale.
es.addEventListener('open', () => {
  loadList().catch((e) => console.error(e));
  loadDiagnostics().catch((e) => console.error(e));
  loadStorage().catch((e) => console.error(e));
});
es.addEventListener('label', (e) => {
  try { prependRow(JSON.parse(e.data)); } catch (err) { console.error('bad label event', err); }
});

// ---------- direct ZPL playground ----------
// POST /api/render: parse + render arbitrary ZPL text, PNG inline. The
// label is NOT archived (playground semantics), so repeated edits don't
// pollute the label list.
const playZpl = $('#play-zpl');
const playGo = $('#play-go');
const playStatus = $('#play-status');
const playImgWrap = $('#play-img-wrap');
const playImg = $('#play-img');

async function playRender() {
  const text = playZpl.value;
  if (!text.trim()) {
    playStatus.textContent = 'Enter ZPL first.';
    return;
  }
  playGo.disabled = true;
  playStatus.textContent = 'rendering...';
  try {
    const r = await fetch('/api/render', {
      method: 'POST',
      headers: { 'content-type': 'text/plain' },
      body: new TextEncoder().encode(text), // binary-unsafe textarea: bytes survive
    });
    if (!r.ok) {
      const err = await r.text().catch(() => '');
      let msg = `HTTP ${r.status}`;
      try { msg += ` - ${JSON.parse(err).error ?? ''}`; } catch { /* non-JSON body */ }
      playStatus.textContent = msg;
      return;
    }
    const blob = await r.blob();
    playImg.src = URL.createObjectURL(blob);
    playImgWrap.hidden = false;
    playStatus.textContent =
      `${r.headers.get('x-label-width')} \u00d7 ${r.headers.get('x-label-height')} dots`;
  } catch (e) {
    playStatus.textContent = `render failed: ${e.message}`;
  } finally {
    playGo.disabled = false;
  }
}

playGo.addEventListener('click', playRender);
playZpl.addEventListener('keydown', (e) => {
  if ((e.ctrlKey || e.metaKey) && e.key === 'Enter') playRender();
});
es.addEventListener('state', (e) => {
  try { setStateValues(JSON.parse(e.data)); } catch (err) { console.error('bad state event', err); }
});

// ---------- boot ----------
buildStatePanel();
getJSON('/api/printer-state').then(setStateValues).catch((e) => console.error(e));
loadDiagnostics().catch((e) => console.error(e));
loadStorage().catch((e) => console.error(e));
loadList().catch((e) => console.error(e));
