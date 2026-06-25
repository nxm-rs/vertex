// Inflight-cap sweep: single ra-mode range download of the large file, byte-verify
// against the known oracle sha, parse the final retrieval-instrumentation line for
// serving-peer count / per-peer concentration / throttle decomposition / topo, and
// report aggregate kbps. One config per invocation (URL params drive inflight/pf/tt).
//
// Env: DEMO_URL (with ?ra&debug&inflight=N...), REF, OFFSET, LEN, WANT_SHA, OUT, LABEL.
import pw from '/nix/store/1i3ahl6fk8llj3f0qnpzmi6rvks5fxdi-playwright-test-1.59.1/lib/node_modules/playwright-core/index.js';
const { chromium } = pw;
import fs from 'node:fs';
import readline from 'node:readline';

const REF = process.env.REF || '0x00850a14dbf6a663be16679f99d4824ca634cff8cce3015b9940a0e905e8a0bf';
const OFFSET = Number(process.env.OFFSET || 50000000);
const LEN = Number(process.env.LEN || 60000000);
const WANT_SHA = process.env.WANT_SHA || '0f287ef878ef8141b9768eaf19f2bf9025d2543eaf77774e4567fa37eee2cbad';
const WIDTH = Number(process.env.WIDTH || 0);
const LABEL = process.env.LABEL || 'run';
const URL = process.env.DEMO_URL || `http://127.0.0.1:8099/?ra&debug`;
const OUT = process.env.OUT || '/tmp/demo-nodefix/inflight-sweep-logs';
const READY_TIMEOUT_MS = Number(process.env.READY_TIMEOUT_MS || 300000);
const DL_TIMEOUT_MS = Number(process.env.DL_TIMEOUT_MS || 900000);
const READY_PEERS = Number(process.env.READY_PEERS || 60);
fs.mkdirSync(OUT, { recursive: true });

const t0 = Date.now();
const ts = () => `+${((Date.now() - t0) / 1000).toFixed(3)}s`;
const consoleLog = fs.createWriteStream(`${OUT}/console.log`, { flags: 'w' });

const CHROME = '/nix/store/apgrh70j3xvl7pz63nq2rx19l5zmz7q8-playwright-browsers/chromium-1217/chrome-linux64/chrome';
const browser = await chromium.launch({ headless: true, executablePath: CHROME, args: ['--no-sandbox', '--disable-dev-shm-usage', '--js-flags=--expose-gc'] });
const ctx = await browser.newContext();
const page = await ctx.newPage();
page.on('console', (msg) => { try { consoleLog.write(`${ts()} [${msg.type()}] ${msg.text()}\n`); } catch {} });
page.on('pageerror', (err) => { try { consoleLog.write(`${ts()} [pageerror] ${err.message}\n`); } catch {} });

await page.goto(URL, { waitUntil: 'domcontentloaded' });

const bootStart = Date.now();
let booted = null;
while (Date.now() - bootStart < READY_TIMEOUT_MS) {
  const r = await page.evaluate(() => (window.__swarmDemo ? window.__swarmDemo.readiness() : null)).catch(() => null);
  if (r && r.depth >= 3 && r.connectedPeers >= READY_PEERS) { booted = r; console.log(`${ts()} READY depth=${r.depth} peers=${r.connectedPeers}`); break; }
  await page.waitForTimeout(2000);
}
if (!booted) console.log(`${ts()} NEVER READY (continuing anyway)`);

// Let topology settle a few more seconds so the connected footprint is steady.
await page.waitForTimeout(8000);
const preConnected = await page.evaluate(() => (window.__swarmDemo ? window.__swarmDemo.readiness().connectedPeers : null)).catch(() => null);
console.log(`${ts()} ${LABEL} range OFFSET=${OFFSET} LEN=${LEN} preConnected=${preConnected}`);

const r = await page.evaluate(async ({ ref, offset, len, width, timeout }) => {
  const sha256 = async (buf) => {
    const d = await crypto.subtle.digest('SHA-256', buf);
    return [...new Uint8Array(d)].map((b) => b.toString(16).padStart(2, '0')).join('');
  };
  const c = window.__swarmDemo.client;
  delete window.showSaveFilePicker;
  window.__swarmRaSaveHandle = null;
  let sha = null, len2 = null, err = null;
  const t = performance.now();
  try {
    const sinkVal = await window.createRandomAccessSink('swarm-range.bin', len);
    const out = await Promise.race([
      c.streamToSinkRandomAccessRange(ref, offset, len, width, sinkVal).then(() => ({ ok: true })),
      new Promise((rs) => setTimeout(() => rs({ ok: false, err: 'timeout' }), timeout)),
    ]);
    if (!out.ok) err = out.err;
    const buf = await window.readBackOpfsStaged();
    if (buf) { len2 = buf.byteLength; sha = await sha256(buf); }
  } catch (e) { err = String(e && e.message ? e.message : e); }
  const ms = performance.now() - t;
  return { ms, sha, len: len2, err };
}, { ref: REF, offset: OFFSET, len: LEN, width: WIDTH, timeout: DL_TIMEOUT_MS }).catch((e) => ({ fatal: String(e) }));

const secs = (r.ms || 1) / 1000;
const kbps = r.len ? Number(((r.len / 1024) / secs).toFixed(2)) : null;
const shaMatch = r.sha === WANT_SHA;
const lenMatch = r.len === LEN;

// Parse the LAST few retrieval-instrumentation lines for the steady-state serving
// footprint (the early lines ramp up; the tail lines reflect the file finishing).
// Take a mid-download line (around 60% of instrumentation lines) as the steady-state
// representative, plus the max conc_peers seen.
//
// Stream the console log line-by-line rather than readFileSync: a sustained
// large download produces a multi-hundred-MB log that overflows V8's max string
// length (ERR_STRING_TOO_LONG). A streaming line reader keeps memory flat and
// the per-line state (instr rows, churn counters) is accumulated as we go.
const re = /retrieval-instrumentation (.+?) color:/;
const instr = [];
let brokenPipe = 0, ioError = 0;
const ioRe = /\bIo\(|\bIO error|connection.*closed|reset by peer/i;
await new Promise((resolve, reject) => {
  const rl = readline.createInterface({
    input: fs.createReadStream(`${OUT}/console.log`, { encoding: 'utf8' }),
    crlfDelay: Infinity,
  });
  rl.on('line', (line) => {
    if (line.includes('BrokenPipe')) brokenPipe++;
    if (ioRe.test(line)) ioError++;
    const m = line.match(re);
    if (!m) return;
    const fields = {};
    for (const kv of m[1].trim().split(/\s+/)) {
      const eq = kv.indexOf('=');
      if (eq > 0) fields[kv.slice(0, eq)] = kv.slice(eq + 1);
    }
    instr.push(fields);
  });
  rl.on('close', resolve);
  rl.on('error', reject);
});
const num = (f, k) => (f && f[k] != null ? Number(f[k]) : null);
let maxConc = 0, maxConnected = 0;
const concSeries = [], connectedSeries = [];
for (const f of instr) {
  const cp = num(f, 'conc_peers'); if (cp != null) { concSeries.push(cp); if (cp > maxConc) maxConc = cp; }
  const tc = num(f, 'topo_connected'); if (tc != null) { connectedSeries.push(tc); if (tc > maxConnected) maxConnected = tc; }
}
// Steady-state representative: median of the middle 50% of instrumentation lines.
const mid = instr.slice(Math.floor(instr.length * 0.25), Math.ceil(instr.length * 0.75));
const medianField = (k) => {
  const vals = mid.map((f) => num(f, k)).filter((v) => v != null).sort((a, b) => a - b);
  return vals.length ? vals[Math.floor(vals.length / 2)] : null;
};
const steady = {
  conc_peers: medianField('conc_peers'),
  conc_max: medianField('conc_max'),
  conc_top10_share: medianField('conc_top10_share'),
  topo_connected: medianField('topo_connected'),
  topo_routing: medianField('topo_routing'),
  throttle_wait_ms_mean: medianField('throttle_wait_ms_mean'),
  throttle_sleep_ms_mean: medianField('throttle_sleep_ms_mean'),
  rtt_ms_mean: medianField('rtt_ms_mean'),
  throttle_capped: medianField('throttle_capped'),
  leg_remote: medianField('leg_remote'),
  leg_busy: medianField('leg_busy'),
};
// Churn counters (brokenPipe / ioError) were accumulated during the streaming
// parse above, so no second pass over the log is needed.

const last = instr[instr.length - 1] || {};
const lastFields = {};
for (const k of ['served', 'legs', 'ps_offers', 'ps_offered_au', 'ps_accepted_au', 'ps_full', 'ps_partial', 'max_peer_debt', 'debt_gated', 'leg_remote', 'leg_protocol', 'leg_notconn', 'leg_chanclosed', 'leg_cancelled']) lastFields[k] = num(last, k);

// Per-peer chunks/s estimate: served chunks over wall seconds, divided by the
// steady serving-peer count, to test the ~2.1 forgiveness-cap.
const servedTotal = num(last, 'served') || 0;
const perPeerChunksPerSec = (steady.conc_peers && secs) ? Number((servedTotal / secs / steady.conc_peers).toFixed(3)) : null;
const aggChunksPerSec = secs ? Number((servedTotal / secs).toFixed(2)) : null;

const summary = {
  label: LABEL, ref: REF, range: { offset: OFFSET, len: LEN },
  ready: booted ? { depth: booted.depth, peers: booted.connectedPeers } : null,
  preConnected,
  byteVerify: { sha: r.sha, wantSha: WANT_SHA, shaMatch, len: r.len, wantLen: LEN, lenMatch, err: r.err || null },
  throughput: { seconds: Number(secs.toFixed(2)), kbps, mbps: kbps ? Number((kbps / 1024).toFixed(3)) : null },
  serving: { steady_conc_peers: steady.conc_peers, max_conc_peers: maxConc, steady_conc_max: steady.conc_max, conc_top10_share: steady.conc_top10_share },
  footprint: { steady_topo_connected: steady.topo_connected, max_topo_connected: maxConnected, steady_topo_routing: steady.topo_routing },
  forgivenessModel: { aggChunksPerSec, perPeerChunksPerSec, servedTotal },
  // Compliance proof: the max per-peer debt the gate observed (must be under the
  // light disconnect line 1,687,500), how many admissions the gate refused, and
  // whether outbound settlement kept firing through the whole download (a frozen
  // ps_offers count between the mid and last lines means settlement starved).
  debtGate: {
    maxPeerDebt: lastFields.max_peer_debt,
    debtGated: lastFields.debt_gated,
    underDisconnectLine: lastFields.max_peer_debt != null ? lastFields.max_peer_debt < 1687500 : null,
  },
  settlement: {
    ps_offers: lastFields.ps_offers,
    ps_offered_au: lastFields.ps_offered_au,
    ps_accepted_au: lastFields.ps_accepted_au,
    ps_full: lastFields.ps_full,
    ps_partial: lastFields.ps_partial,
    ps_offers_mid: medianField('ps_offers'),
    // Settlement kept pace if offers grew over the back half of the download.
    ps_kept_firing: (lastFields.ps_offers != null && medianField('ps_offers') != null)
      ? lastFields.ps_offers > medianField('ps_offers') : null,
  },
  pacing: { throttle_wait_ms_mean: steady.throttle_wait_ms_mean, throttle_sleep_ms_mean: steady.throttle_sleep_ms_mean, rtt_ms_mean: steady.rtt_ms_mean, throttle_capped: steady.throttle_capped },
  churn: { brokenPipe, ioError, leg_remote: steady.leg_remote, leg_busy: steady.leg_busy, lastLeg: lastFields },
  instrLines: instr.length,
  VERDICT: (shaMatch && lenMatch) ? 'BYTE-EXACT' : 'FAIL',
};
console.log(`${ts()} SUMMARY ${JSON.stringify(summary)}`);
fs.writeFileSync(`${OUT}/summary.json`, JSON.stringify(summary, null, 2));
consoleLog.end();
await browser.close();
process.exit(summary.VERDICT === 'BYTE-EXACT' ? 0 : 1);
