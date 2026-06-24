import pw from '/nix/store/1i3ahl6fk8llj3f0qnpzmi6rvks5fxdi-playwright-test-1.59.1/lib/node_modules/playwright-core/index.js';
const { chromium } = pw;
const URL = 'http://127.0.0.1:8099/shard.html';
const REF = '439fdc2c403dfedc3bde7ea122240856ca13c6417b4dc26d705e808f37bc1fe9';
const PATH = '__up__/hoverfly/hoverfly_bg.wasm';
const CHROME = '/nix/store/apgrh70j3xvl7pz63nq2rx19l5zmz7q8-playwright-browsers/chromium-1217/chrome-linux64/chrome';
const browser = await chromium.launch({ headless: true, executablePath: CHROME, args: ['--no-sandbox', '--disable-dev-shm-usage'] });
const page = await (await browser.newContext()).newPage();
page.on('console', (m) => { const t = m.text(); if (t.includes('PROBE')) console.log(t); });
await page.goto(URL, { waitUntil: 'domcontentloaded' });
await page.waitForFunction(() => typeof window.__shardAddrDownload === 'function', { timeout: 30000 });

const res = await page.evaluate(async ({ ref, path }) => {
  // Boot one biased-off worker, resolve, list leaves, report offset coverage.
  const w = new Worker('/worker-node.js', { type: 'module' });
  let nextId = 1; const pending = new Map();
  w.addEventListener('message', (e) => { const m = e.data || {}; const p = pending.get(m.id); if (p) { pending.delete(m.id); m.type === 'error' ? p.reject(new Error(m.err)) : p.resolve(m); } });
  const call = (payload) => new Promise((res, rej) => { const id = nextId++; pending.set(id, { resolve: res, reject: rej }); w.postMessage({ ...payload, id }); });
  await call({ type: 'boot' });
  await new Promise((r) => setTimeout(r, 18000));
  const rootMsg = await call({ type: 'resolvePath', address: ref, path });
  const fileRoot = rootMsg.fileRoot;
  const sizeMsg = await call({ type: 'size', fileRoot });
  const total = sizeMsg.size;
  const lm = await call({ type: 'listLeaves', fileRoot });
  const flat = lm.leaves || [];
  const offs = [];
  for (let i = 1; i < flat.length; i += 2) offs.push(flat[i]);
  offs.sort((a, b) => a - b);
  const dupes = []; const seen = new Set();
  for (const o of offs) { if (seen.has(o)) dupes.push(o); seen.add(o); }
  // Gaps: expected offsets are 0,4096,8192,... up to total.
  const expected = Math.ceil(total / 4096);
  const missing = [];
  for (let i = 0; i < expected; i++) { if (!seen.has(i * 4096)) missing.push(i * 4096); }
  w.terminate();
  return { total, leaves: offs.length, expected, distinct: seen.size, dupeCount: dupes.length, dupesSample: dupes.slice(0, 8), missingCount: missing.length, missingSample: missing.slice(0, 12), maxOff: offs[offs.length - 1], firstFew: offs.slice(0, 8) };
}, { ref: REF, path: PATH });
console.log('PROBE ' + JSON.stringify(res));
await Promise.race([browser.close().catch(() => {}), new Promise((r) => setTimeout(r, 4000))]);
process.exit(0);
