import pw from '/nix/store/1i3ahl6fk8llj3f0qnpzmi6rvks5fxdi-playwright-test-1.59.1/lib/node_modules/playwright-core/index.js';
const { chromium } = pw;
import fs from 'node:fs';

const URL = process.env.DEMO_URL || 'http://127.0.0.1:8099/shard.html';
const OUT = process.env.OUT || '/tmp/demo-nodefix/shard-range-logs';
const REF = process.env.REF || '0xdda8d67ddafd9421d84560ebaa99384e069a7d700c7e971f9865451164d10e6a';
const PATH = process.env.PATH_IN_MANIFEST || '';
const K = Number(process.env.K || 3);
const WIDTH = Number(process.env.WIDTH || 0);
const MODE = process.env.MODE || 'range'; // 'range' (byte) or 'addr' (address-shard)
const FOOTPRINT = Number(process.env.FOOTPRINT || 0);
const BOOTSTRAP = Number(process.env.BOOTSTRAP || 0);
const WARMUP = Number(process.env.WARMUP || 16000);
const RUN_TIMEOUT_MS = Number(process.env.RUN_TIMEOUT_MS || 240000);
const HARD_TIMEOUT_MS = Number(process.env.HARD_TIMEOUT_MS || 360000);
fs.mkdirSync(OUT, { recursive: true });

const t0 = Date.now();
const ts = () => `+${((Date.now() - t0) / 1000).toFixed(3)}s`;
const consoleLog = fs.createWriteStream(`${OUT}/console.log`, { flags: 'w' });

const CHROME = '/nix/store/apgrh70j3xvl7pz63nq2rx19l5zmz7q8-playwright-browsers/chromium-1217/chrome-linux64/chrome';
const browser = await chromium.launch({ headless: true, executablePath: CHROME, args: ['--no-sandbox', '--disable-dev-shm-usage'] });
const ctx = await browser.newContext();
const page = await ctx.newPage();

const wsState = new Map(); let maxWs = 0;
page.on('websocket', (ws) => { wsState.set(ws, ws); const live = countOpenWs(); if (live > maxWs) maxWs = live; });
const countOpenWs = () => { let n = 0; for (const [ws] of wsState) if (!ws.isClosed()) n++; return n; };

page.on('console', (msg) => { try { consoleLog.write(`${ts()} [${msg.type()}] ${msg.text()}\n`); } catch {} });
page.on('pageerror', (err) => { try { consoleLog.write(`${ts()} [pageerror] ${err.message}\n`); } catch {} });

await page.goto(URL, { waitUntil: 'domcontentloaded' });
const fnName = MODE === 'addr' ? '__shardAddrDownload' : '__shardRangeDownload';
await page.waitForFunction((n) => typeof window[n] === 'function', fnName, { timeout: 30000 });

console.log(`${ts()} shard ${MODE.toUpperCase()} download REF=${REF} PATH=${PATH} K=${K} WIDTH=${WIDTH} FOOTPRINT=${FOOTPRINT} BOOTSTRAP=${BOOTSTRAP} WARMUP=${WARMUP}`);

const evalP = page.evaluate(async ({ ref, k, warmup, runTimeout, width, path, mode, footprint, bootstrap }) => {
  try {
    if (mode === 'addr') {
      return await window.__shardAddrDownload(ref, k, warmup, runTimeout, width, path, footprint, bootstrap);
    }
    return await window.__shardRangeDownload(ref, k, warmup, runTimeout, width, path, footprint, bootstrap);
  } catch (e) {
    return { error: String(e && e.message ? e.message : e) };
  }
}, { ref: REF, k: K, warmup: WARMUP, runTimeout: RUN_TIMEOUT_MS, width: WIDTH, path: PATH, mode: MODE, footprint: FOOTPRINT, bootstrap: BOOTSTRAP });

const result = await Promise.race([
  evalP,
  new Promise((res) => setTimeout(() => res({ error: 'harness-hard-timeout' }), HARD_TIMEOUT_MS)),
]);

result.maxWs = maxWs;
result.totalWsOpened = wsState.size;

// Scrape the per-worker retrieval-instrumentation lines for the not-connected
// tax: each worker logs cumulative leg counts; take the last line per worker
// (highest served) and aggregate the leg-outcome shares.
try {
  const text = fs.readFileSync(`${OUT}/console.log`, 'utf8');
  const lines = text.split('\n').filter((l) => l.includes('retrieval-instrumentation'));
  // The lines interleave across workers; the cumulative counters are per-worker
  // (separate wasm instances) but the console does not tag the worker. Aggregate
  // the maxima of each counter as a coarse fleet view, plus the final-line shares.
  let legs = 0, served = 0, notconn = 0, remote = 0, notfound = 0, timeout = 0, busy = 0;
  for (const l of lines) {
    const g = (k) => { const m = l.match(new RegExp(k + '=(\\d+)')); return m ? Number(m[1]) : 0; };
    // Keep the running maxima (counters only grow within a worker; across
    // workers this sums the leading worker's view, a fleet-scale proxy).
    legs = Math.max(legs, g('legs'));
    served = Math.max(served, g('served'));
    notconn = Math.max(notconn, g('leg_notconn'));
    remote = Math.max(remote, g('leg_remote'));
    notfound = Math.max(notfound, g('leg_notfound'));
    timeout = Math.max(timeout, g('leg_timeout'));
    busy = Math.max(busy, g('leg_busy'));
  }
  const totalLegs = legs || 1;
  result.legStats = {
    legs, served, notconn, remote, notfound, timeout, busy,
    substreamsPerChunk: served ? Number((legs / served).toFixed(2)) : null,
    notconnPct: Number((100 * notconn / totalLegs).toFixed(1)),
    sampleLines: lines.length,
  };
} catch (e) { result.legStatsError = String(e && e.message ? e.message : e); }

console.log(`${ts()} ${MODE.toUpperCase()}-RESULT ${JSON.stringify(result)}`);
fs.writeFileSync(`${OUT}/result.json`, JSON.stringify(result, null, 2));
consoleLog.end();
// The wasm node's background tasks/websockets can stall browser.close() for
// minutes after the result is written; bound it and hard-exit so a serial sweep
// is not held hostage by teardown.
await Promise.race([
  browser.close().catch(() => {}),
  new Promise((res) => setTimeout(res, 5000)),
]);
process.exit(0);
