// peer-feed.js
//
// The single decoupling seam between the UI and its data source.
//
// The scanner UI (globe backdrop + scoreboard + self panel) consumes ONE event
// stream through the PeerFeed interface below. Two implementations satisfy it:
//
//   - MockPeerFeed  : synthetic peers with periodic connects/disconnects and
//                     score churn. Drives the whole UI standalone, no wasm node
//                     required. Default today.
//   - WasmPeerFeed  : the future real source, reading from the wasm `SwarmDemo`
//                     handle. STUBBED: there is exactly ONE clearly marked
//                     integration point to wire when the wasm methods exist.
//
// Pick with the URL: `?mock` (default) or `?live`.
//
// ---------------------------------------------------------------------------
// EVENT / DATA CONTRACT
// ---------------------------------------------------------------------------
// A feed is an EventTarget-like object exposing on(event, handler) and start()
// / stop(). It emits these events with these payloads:
//
//   'self'      -> { overlay, peerId }
//                  (the local node; emitted once on start)
//
//   'connect'   -> { overlay, peerId, multiaddrs:[string],
//                    po /* 0..31 proximity order */,
//                    score:{ total:number, ... }, connectedAt /* ms epoch */ }
//
//   'score'     -> { overlay, score:{ total:number, ... } }
//
//   'disconnect'-> { overlay }
//
// The UI treats `overlay` as the peer's stable key. `score.total` drives the
// color gradient and the scoreboard sort. `po` is the Kademlia proximity order
// / bin (0 far .. 31 near).
// ---------------------------------------------------------------------------

/**
 * Minimal typed event emitter shared by both feeds.
 * @template T
 */
class Emitter {
  constructor() {
    /** @type {Map<string, Set<Function>>} */
    this._handlers = new Map();
  }
  on(type, fn) {
    if (!this._handlers.has(type)) this._handlers.set(type, new Set());
    this._handlers.get(type).add(fn);
    return () => this._handlers.get(type)?.delete(fn);
  }
  emit(type, payload) {
    const hs = this._handlers.get(type);
    if (hs) for (const fn of hs) {
      try { fn(payload); } catch (e) { console.error('feed handler error', e); }
    }
  }
}

// ===========================================================================
// MockPeerFeed
// ===========================================================================

const HEX = '0123456789abcdef';
const rand = (n) => Math.floor(Math.random() * n);
const pick = (arr) => arr[rand(arr.length)];

function randHex(len) {
  let s = '';
  for (let i = 0; i < len; i++) s += HEX[rand(16)];
  return s;
}
function randOverlay() {
  return '0x' + randHex(64);
}
function randPeerId() {
  // libp2p peer ids look like 16Uiu2HA...; good enough for display.
  return '16Uiu2HA' + randHexBase58(44);
}
function randHexBase58(len) {
  const B58 = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';
  let s = '';
  for (let i = 0; i < len; i++) s += B58[rand(B58.length)];
  return s;
}
function randIp() {
  // Avoid reserved ranges for plausibility; not used for real lookups in mock.
  return `${1 + rand(223)}.${rand(256)}.${rand(256)}.${1 + rand(254)}`;
}

/**
 * Generates a lively synthetic peer population so the full UI is demoable with
 * no wasm node. Connects/disconnects on a cadence and churns scores.
 */
export class MockPeerFeed extends Emitter {
  /**
   * @param {object} [opts]
   * @param {number} [opts.initial=14]  peers connected at startup
   * @param {number} [opts.max=26]      population ceiling
   * @param {number} [opts.tickMs=1800] churn interval
   */
  constructor(opts = {}) {
    super();
    this.kind = 'mock';
    this.initial = opts.initial ?? 14;
    this.max = opts.max ?? 26;
    this.tickMs = opts.tickMs ?? 1800;
    /** @type {Map<string, any>} */
    this.peers = new Map();
    this._timer = null;
  }

  start() {
    this.self = {
      overlay: randOverlay(),
      peerId: randPeerId(),
    };
    this.emit('self', this.self);

    for (let i = 0; i < this.initial; i++) this._spawnPeer();

    this._timer = setInterval(() => this._tick(), this.tickMs);
    return this;
  }

  stop() {
    if (this._timer) clearInterval(this._timer);
    this._timer = null;
  }

  _spawnPeer() {
    if (this.peers.size >= this.max) return;
    const overlay = randOverlay();
    const peer = {
      overlay,
      peerId: randPeerId(),
      multiaddrs: [`/dns4/${randIp().replace(/\./g, '-')}.${randHexBase58(46)}.libp2p.direct/tcp/443/wss`],
      po: 4 + rand(28), // 4..31, weighted toward nearer bins
      score: { total: Math.round((30 + Math.random() * 70) * 10) / 10, latency: 20 + rand(180) },
      connectedAt: Date.now(),
    };
    this.peers.set(overlay, peer);
    this.emit('connect', peer);
  }

  _tick() {
    const n = this.peers.size;
    const roll = Math.random();

    // Bias toward growth until we near the ceiling, then balance.
    if (n < this.initial || (n < this.max && roll < 0.45)) {
      this._spawnPeer();
    } else if (n > 4 && roll < 0.62) {
      // disconnect a random peer
      const overlay = pick([...this.peers.keys()]);
      this.peers.delete(overlay);
      this.emit('disconnect', { overlay });
    }

    // Score churn on a couple of random peers each tick.
    const keys = [...this.peers.keys()];
    for (let i = 0; i < Math.min(3, keys.length); i++) {
      const overlay = pick(keys);
      const p = this.peers.get(overlay);
      if (!p) continue;
      const delta = (Math.random() - 0.5) * 12;
      p.score = {
        ...p.score,
        total: Math.max(0, Math.min(100, Math.round((p.score.total + delta) * 10) / 10)),
        latency: Math.max(5, (p.score.latency || 60) + rand(40) - 20),
      };
      this.emit('score', { overlay, score: p.score });
    }
  }
}

// ===========================================================================
// WasmPeerFeed  (STUB: future real source)
// ===========================================================================

/**
 * Reads peer events from the live wasm `SwarmDemo` handle.
 *
 * !!! NOT YET FUNCTIONAL !!!
 *
 * This is the future real source. The wasm side does not yet expose the JS
 * methods this needs (`self()` and `drainPeerEvents()` on the SwarmDemo
 * handle); wiring those in Rust is a SEPARATE later step. Until then, selecting
 * `?live` will fall back to the mock with a console warning (see makePeerFeed).
 */
export class WasmPeerFeed extends Emitter {
  /**
   * @param {object} deps
   * @param {object} deps.handle  the wasm SwarmDemo handle (window.__swarmDemo)
   * @param {number} [deps.pollMs=1000]
   */
  constructor(deps) {
    super();
    this.kind = 'wasm';
    this.handle = deps.handle;
    this.pollMs = deps.pollMs ?? 1000;
    this._timer = null;
    this._known = new Set();
  }

  async start() {
    // ---- Self ----
    try {
      // ===================================================================
      // SINGLE REAL-DATA INTEGRATION POINT (self).
      // When the wasm handle exposes `self()` returning { overlay, peerId },
      // read it here. Today only `overlay` exists as a getter.
      // ===================================================================
      const overlay = this.handle?.overlay ?? null;
      this.emit('self', {
        overlay,
        peerId: this.handle?.self?.()?.peerId ?? null,
      });
    } catch (e) {
      console.warn('WasmPeerFeed self() failed', e);
    }

    this._timer = setInterval(() => this._poll(), this.pollMs);
    return this;
  }

  stop() {
    if (this._timer) clearInterval(this._timer);
    this._timer = null;
  }

  async _poll() {
    // =====================================================================
    // SINGLE REAL-DATA INTEGRATION POINT (peers).
    //
    // TODO(wasm): the SwarmDemo handle must expose `drainPeerEvents()` that
    // returns an array of raw peer events since the last call, each shaped:
    //   { type:'connect'|'score'|'disconnect', overlay, peerId,
    //     multiaddrs:[], po, score:{total}, connectedAt }
    // The wasm side already buffers TopologyEvents (see src/lib.rs
    // drainEvents); a sibling `drainPeerEvents` that maps PeerReady/
    // PeerDisconnected/PingCompleted into the above shape is all that's
    // needed. Until that exists this method is a no-op.
    // =====================================================================
    const drain = this.handle && this.handle.drainPeerEvents;
    if (typeof drain !== 'function') return; // not wired yet

    let raw;
    try { raw = drain.call(this.handle); } catch (e) { console.warn(e); return; }
    if (!raw || !raw.length) return;

    for (const e of raw) {
      if (e.type === 'connect') {
        this.emit('connect', {
          overlay: e.overlay,
          peerId: e.peerId ?? null,
          multiaddrs: e.multiaddrs || [],
          po: e.po ?? 0,
          score: e.score ?? { total: 0 },
          connectedAt: e.connectedAt ?? Date.now(),
        });
        this._known.add(e.overlay);
      } else if (e.type === 'score') {
        this.emit('score', { overlay: e.overlay, score: e.score ?? { total: 0 } });
      } else if (e.type === 'disconnect') {
        this.emit('disconnect', { overlay: e.overlay });
        this._known.delete(e.overlay);
      }
    }
  }
}

// ===========================================================================
// Factory
// ===========================================================================

/**
 * Build the active feed from the URL switch.
 *   ?live -> WasmPeerFeed (falls back to mock if the wasm handle isn't ready)
 *   ?mock or default -> MockPeerFeed
 *
 * @param {object} deps  (see WasmPeerFeed ctor) plus { handle }
 * @returns {{ feed: MockPeerFeed|WasmPeerFeed, mode: string }}
 */
export function makePeerFeed(deps = {}) {
  const search = (typeof location !== 'undefined' ? location.search : '').toLowerCase();
  const wantLive = search.includes('live');

  if (wantLive && deps.handle && typeof deps.handle.drainPeerEvents === 'function') {
    return { feed: new WasmPeerFeed(deps), mode: 'live' };
  }
  if (wantLive) {
    console.warn(
      '[peer-feed] ?live requested but the wasm SwarmDemo handle does not yet ' +
      'expose drainPeerEvents(); falling back to MockPeerFeed. Wire the Rust ' +
      'side (see WasmPeerFeed integration points) then ?live will go real.',
    );
  }
  return { feed: new MockPeerFeed(), mode: 'mock' };
}
