// app.js
//
// Presentation-layer orchestrator for the Vertex Swarm scanner UI.
//
// Wires a peer feed (mock today, wasm later) to three views:
//   - a 3D globe (globe.gl, vendored) rendering the earth and country outlines
//     as a backdrop
//   - a live, sortable peer scoreboard
//   - a self panel for our own node
//
// The Rust wasm app independently mounts the topology stats and the Files
// upload/download panel into #topo-mount / #files-mount (see src/ui.rs,
// src/files_ui.rs); this file does not touch those, it only owns the scanner
// chrome and the peer feed.
//
// Everything here runs against the abstract PeerFeed contract, so swapping the
// mock for the real wasm source is a one-line change in peer-feed.js.

import { makePeerFeed } from './peer-feed.js';

// Paths are resolved relative to this module's own URL so they work whether the
// app is served from site root (/) or a subpath (GitHub Pages /vertex/).
const ASSET_BASE = new URL('.', import.meta.url); // .../assets/
const assetUrl = (rel) => new URL(rel, ASSET_BASE).href;

// Optional dark earth texture. Left unset by default: we render a solid dark
// sphere + country polygons (the scanner look) and ship no large texture, so
// there is no subresource to 404. To use a texture, vendor it into assets/ and
// set GLOBE_IMG to assetUrl('your-texture.jpg').
const GLOBE_IMG = null;

// Coerce a score (number or { total }) to a clamped 0..100 number.
function scoreNum(score) {
  const n = typeof score === 'object' && score !== null ? score.total : score;
  return Number.isFinite(n) ? Math.max(0, Math.min(100, n)) : 0;
}

// ---- score -> color gradient (low=red .. mid=amber .. high=cyan/green) -----
function scoreColor(score, alpha = 1) {
  const t = scoreNum(score) / 100;
  // interpolate red -> amber -> cyan
  let r, g, b;
  if (t < 0.5) {
    const k = t / 0.5;
    r = 255; g = Math.round(60 + 150 * k); b = 60;
  } else {
    const k = (t - 0.5) / 0.5;
    r = Math.round(255 - 205 * k); g = Math.round(210 + 20 * k); b = Math.round(60 + 175 * k);
  }
  return `rgba(${r},${g},${b},${alpha})`;
}

function shortOverlay(overlay) {
  if (!overlay) return '-';
  const s = overlay.startsWith('0x') ? overlay.slice(2) : overlay;
  if (s.length <= 12) return overlay;
  return `0x${s.slice(0, 4)}…${s.slice(-4)}`;
}

function fmtUptime(ms) {
  const s = Math.max(0, Math.floor((Date.now() - ms) / 1000));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${s % 60}s`;
  const h = Math.floor(m / 60);
  return `${h}h ${m % 60}m`;
}

// ===========================================================================
// State
// ===========================================================================

/** overlay -> peer record */
const peers = new Map();
let self = null;
let sortKey = 'score';
let sortDir = -1; // -1 desc, 1 asc

// ===========================================================================
// Globe
// ===========================================================================

let globe = null;

function initGlobe(el) {
  if (typeof Globe === 'undefined') {
    console.error('[app] globe.gl global not found - vendored bundle failed to load');
    el.innerHTML = '<div class="globe-fallback">globe library unavailable</div>';
    return;
  }
  globe = Globe()(el)
    .backgroundColor('rgba(0,0,0,0)')
    .showAtmosphere(true)
    .atmosphereColor('#2bd9ff')
    .atmosphereAltitude(0.18)
    .showGraticules(true);

  // Use a vendored dark earth texture if one is configured; otherwise render
  // the globe as a solid dark sphere with country polygons (the default look).
  if (GLOBE_IMG) globe.globeImageUrl(GLOBE_IMG);

  // Solid dark base so the globe reads even without a texture. The material is
  // a real three.js material; mutate its existing Color objects in place via
  // .set() rather than constructing new THREE.Color (THREE is bundled inside
  // globe.gl and not exposed as a global under COEP).
  try {
    const mat = globe.globeMaterial();
    mat.color && mat.color.set && mat.color.set('#0a1018');
    mat.emissive && mat.emissive.set && mat.emissive.set('#06121c');
    if ('emissiveIntensity' in mat) mat.emissiveIntensity = 0.4;
    mat.needsUpdate = true;
  } catch (e) {
    console.warn('[app] globe material tint skipped', e);
  }

  // Auto-rotate.
  const controls = globe.controls();
  controls.autoRotate = true;
  controls.autoRotateSpeed = 0.55;
  controls.enableZoom = true;

  // Optional country polygons for a faint land outline; loaded async.
  loadCountryPolygons();

  // Responsive sizing.
  const resize = () => {
    const rect = el.getBoundingClientRect();
    globe.width(rect.width).height(rect.height);
  };
  resize();
  window.addEventListener('resize', resize);
}

async function loadCountryPolygons() {
  try {
    const r = await fetch(assetUrl('vendor/countries-110m.geojson'));
    if (!r.ok) return;
    const geo = await r.json();
    globe
      .polygonsData(geo.features)
      .polygonCapColor(() => 'rgba(20,40,55,0.35)')
      .polygonSideColor(() => 'rgba(0,0,0,0)')
      .polygonStrokeColor(() => 'rgba(43,217,255,0.35)')
      .polygonAltitude(0.006);
  } catch (e) {
    console.warn('[app] country polygons unavailable', e);
  }
}

// ===========================================================================
// Scoreboard
// ===========================================================================

function initScoreboard() {
  document.querySelectorAll('#scoreboard thead th[data-sort]').forEach((th) => {
    th.addEventListener('click', () => {
      const key = th.dataset.sort;
      if (sortKey === key) sortDir = -sortDir;
      else { sortKey = key; sortDir = key === 'score' || key === 'po' ? -1 : 1; }
      renderScoreboard();
    });
  });
  // Live uptime ticks.
  setInterval(renderScoreboard, 1000);
}

function sortedPeers() {
  const arr = [...peers.values()];
  arr.sort((a, b) => {
    let av, bv;
    switch (sortKey) {
      case 'score': av = a.score?.total ?? 0; bv = b.score?.total ?? 0; break;
      case 'po': av = a.po; bv = b.po; break;
      case 'country': av = a.country || ''; bv = b.country || ''; break;
      case 'uptime': av = a.connectedAt; bv = b.connectedAt; break;
      case 'overlay': default: av = a.overlay; bv = b.overlay; break;
    }
    if (av < bv) return -1 * sortDir;
    if (av > bv) return 1 * sortDir;
    return 0;
  });
  return arr;
}

function renderScoreboard() {
  const tbody = document.getElementById('scoreboard-body');
  if (!tbody) return;
  const rows = sortedPeers();
  document.getElementById('peer-count-badge').textContent = String(peers.size);

  // Build a row map to preserve "new"/"leaving" fade animations cheaply by
  // diffing on overlay; simplest robust approach: rebuild but keep transient
  // classes via the peer record's _new flag.
  tbody.innerHTML = rows.map((p) => {
    const sc = p.score?.total ?? 0;
    const cls = [p._new ? 'row-new' : '', p._leaving ? 'row-leaving' : ''].join(' ').trim();
    return `<tr class="${cls}" data-overlay="${p.overlay}">
      <td class="mono">${shortOverlay(p.overlay)}</td>
      <td class="num"><span class="score-pill" style="--c:${scoreColor(sc)}">${Math.round(sc)}</span></td>
      <td class="num">${p.po}</td>
      <td class="num dim">${fmtUptime(p.connectedAt)}</td>
    </tr>`;
  }).join('');
}

// ===========================================================================
// Self panel
// ===========================================================================

function renderSelf() {
  const el = document.getElementById('self-panel-body');
  if (!el || !self) return;
  el.innerHTML = `
    <div class="self-row"><span class="k">overlay</span><span class="v mono">${shortOverlay(self.overlay)}</span></div>
    <div class="self-row"><span class="k">peer id</span><span class="v mono dim">${self.peerId ? self.peerId.slice(0, 16) + '…' : '-'}</span></div>`;
}

// ===========================================================================
// Feed wiring
// ===========================================================================

function markNew(peer) {
  peer._new = true;
  setTimeout(() => { peer._new = false; renderScoreboard(); }, 2500);
}

function wireFeed(feed, mode) {
  document.getElementById('feed-mode-badge').textContent = mode.toUpperCase();

  feed.on('self', (s) => {
    self = { ...self, ...s };
    renderSelf();
  });

  feed.on('connect', (p) => {
    p._new = true;
    peers.set(p.overlay, p);
    markNew(p);
    renderScoreboard();
  });

  feed.on('score', ({ overlay, score }) => {
    const p = peers.get(overlay);
    if (!p) return;
    p.score = score;
    renderScoreboard();
  });

  feed.on('disconnect', ({ overlay }) => {
    const p = peers.get(overlay);
    if (!p) return;
    p._leaving = true;
    renderScoreboard();
    // fade out, then remove
    setTimeout(() => {
      peers.delete(overlay);
      renderScoreboard();
    }, 650);
  });

  feed.start();
}

// ===========================================================================
// Collapsible panels
// ===========================================================================

function initCollapsibles() {
  document.querySelectorAll('[data-collapse]').forEach((btn) => {
    btn.addEventListener('click', () => {
      const target = document.getElementById(btn.dataset.collapse);
      if (!target) return;
      const collapsed = target.classList.toggle('collapsed');
      btn.textContent = collapsed ? '+' : '–';
    });
  });
}

// ===========================================================================
// Boot
// ===========================================================================

function isLiveRequested() {
  return (window.location.search || '').toLowerCase().includes('live');
}

// The wasm handle is published asynchronously: Rust's start() resolves the
// mainnet bootnodes over the network before setting window.__swarmDemo, so the
// handle (and its drainPeerEvents) appears well after DOMContentLoaded. In ?live
// mode, poll for it rather than reading once; fall back to mock on timeout.
function waitForWasmHandle(timeoutMs = 40000, stepMs = 250) {
  return new Promise((resolve) => {
    const ready = (h) => h && typeof h.drainPeerEvents === 'function';
    if (ready(window.__swarmDemo)) return resolve(window.__swarmDemo);
    const t0 = Date.now();
    const id = setInterval(() => {
      if (ready(window.__swarmDemo)) {
        clearInterval(id);
        resolve(window.__swarmDemo);
      } else if (Date.now() - t0 > timeoutMs) {
        clearInterval(id);
        resolve(null);
      }
    }, stepMs);
  });
}

async function boot() {
  const globeEl = document.getElementById('globe');
  if (globeEl) initGlobe(globeEl);
  initScoreboard();
  initCollapsibles();

  // Resolve the wasm handle. In ?live mode wait for the async publish; otherwise
  // use whatever is present (mock path ignores it).
  let handle = window.__swarmDemo;
  if (isLiveRequested()) {
    const badge = document.getElementById('feed-mode-badge');
    if (badge) badge.textContent = 'LIVE…';
    handle = await waitForWasmHandle();
  }

  const { feed, mode } = makePeerFeed({ handle });
  wireFeed(feed, mode);

  // Expose for debugging / the verification harness.
  window.__scanner = { peers, getSelf: () => self, feed, mode };
}

if (document.readyState === 'loading') {
  document.addEventListener('DOMContentLoaded', boot);
} else {
  boot();
}
