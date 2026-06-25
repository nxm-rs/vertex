import pw from '/nix/store/1i3ahl6fk8llj3f0qnpzmi6rvks5fxdi-playwright-test-1.59.1/lib/node_modules/playwright-core/index.js';
const { chromium } = pw;
import fs from 'node:fs';

const URL = process.env.DEMO_URL || 'http://127.0.0.1:8099/shard.html';
const OUT = process.env.OUT || '/tmp/demo-nodefix/shard-window-logs';
const REF = process.env.REF || '0x00850a14dbf6a663be16679f99d4824ca634cff8cce3015b9940a0e905e8a0bf';
const PATH = process.env.PATH_IN_MANIFEST || '';
const K = Number(process.env.K || 1);
const BASE_OFFSET = Number(process.env.BASE_OFFSET || 50000000);
const WINDOW_LEN = Number(process.env.WINDOW_LEN || 60000000);
const WIDTH = Number(process.env.WIDTH || 0);
const FOOTPRINT = Number(process.env.FOOTPRINT || 32);
const BOOTSTRAP = Number(process.env.BOOTSTRAP || 12);
const WARMUP = Number(process.env.WARMUP || 18000);
const EXPECTED_SHA = process.env.EXPECTED_SHA || '';
const RUN_TIMEOUT_MS = Number(process.env.RUN_TIMEOUT_MS || 300000);
const HARD_TIMEOUT_MS = Number(process.env.HARD_TIMEOUT_MS || 420000);
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

await page.goto(URL + '?noglobe', { waitUntil: 'domcontentloaded' });
await page.waitForFunction(() => typeof window.__shardWindowDownload === 'function', null, { timeout: 30000 });

console.log(`${ts()} shard WINDOW REF=${REF} K=${K} BASE=${BASE_OFFSET} LEN=${WINDOW_LEN} WIDTH=${WIDTH} FOOTPRINT=${FOOTPRINT} BOOTSTRAP=${BOOTSTRAP} WARMUP=${WARMUP} EXPECTED_SHA=${EXPECTED_SHA || '(none)'}`);

const evalP = page.evaluate(async (a) => {
  try {
    return await window.__shardWindowDownload(a.ref, a.k, a.base, a.len, a.warmup, a.runTimeout, a.width, a.path, a.footprint, a.bootstrap, a.expectedSha);
  } catch (e) {
    return { error: String(e && e.message ? e.message : e) };
  }
}, { ref: REF, k: K, base: BASE_OFFSET, len: WINDOW_LEN, warmup: WARMUP, runTimeout: RUN_TIMEOUT_MS, width: WIDTH, path: PATH, footprint: FOOTPRINT, bootstrap: BOOTSTRAP, expectedSha: EXPECTED_SHA });

const result = await Promise.race([
  evalP,
  new Promise((res) => setTimeout(() => res({ error: 'harness-hard-timeout' }), HARD_TIMEOUT_MS)),
]);

result.maxWs = maxWs;
result.totalWsOpened = wsState.size;

console.log(`${ts()} WINDOW-RESULT ${JSON.stringify(result)}`);
fs.writeFileSync(`${OUT}/result.json`, JSON.stringify(result, null, 2));
consoleLog.end();
await Promise.race([
  browser.close().catch(() => {}),
  new Promise((res) => setTimeout(res, 5000)),
]);
process.exit(0);
