// Worker-sharded download coordinator (prototype).
//
// Boots K headless worker nodes (each its own identity and peer set), enumerates
// the file's chunk tree on the first ready worker, then partitions chunk
// retrieval across all K workers (chunk -> worker by address) and fetches each
// shard with bounded per-worker concurrency. Returns aggregate throughput.
//
// This measures whether K independent worker nodes aggregate forwarding
// bandwidth past the single-node ceiling, or whether they contend for one
// origin's network budget (Task B / Task C).

let nextId = 1;

function makeWorker() {
  const w = new Worker('/worker-node.js', { type: 'module' });
  const pending = new Map();
  w.addEventListener('message', (e) => {
    const m = e.data || {};
    const p = pending.get(m.id);
    if (!p) return;
    pending.delete(m.id);
    if (m.type === 'error') p.reject(new Error(m.err));
    else p.resolve(m);
  });
  const call = (payload, transfer, timeoutMs) => new Promise((resolve, reject) => {
    const id = nextId++;
    pending.set(id, { resolve, reject });
    if (timeoutMs) {
      setTimeout(() => {
        if (pending.has(id)) { pending.delete(id); reject(new Error('call timeout')); }
      }, timeoutMs);
    }
    w.postMessage({ ...payload, id }, transfer || []);
  });
  return { w, call };
}

// Boot K workers. `opts.prefixBits` biases each worker's overlay into its slice
// (worker i gets prefixValue = i << (8 - prefixBits)); `opts.footprint`/
// `opts.bootstrap` shrink each worker's connection budget so K fit under the
// renderer socket pool. All zero keeps the unbiased full-footprint behaviour.
async function bootWorkers(k, opts) {
  opts = opts || {};
  const prefixBits = opts.prefixBits | 0;
  const footprint = opts.footprint | 0;
  const bootstrap = opts.bootstrap | 0;
  const workers = [];
  for (let i = 0; i < k; i++) workers.push(makeWorker());
  const readies = await Promise.all(workers.map((wk, i) => wk.call({
    type: 'boot',
    prefixBits,
    prefixValue: prefixBits ? (i << (8 - prefixBits)) & 0xff : 0,
    footprint,
    bootstrap,
  })));
  return { workers, overlays: readies.map((r) => r.overlay) };
}

// Worker index for a chunk address by its leading `prefixBits` bits, so a chunk
// is fetched by the worker whose overlay sits in the same address slice.
function workerForAddr(addrHex, prefixBits) {
  const h = addrHex.startsWith('0x') ? addrHex.slice(2) : addrHex;
  const b = parseInt(h.slice(0, 2), 16) || 0;
  return b >> (8 - prefixBits);
}

// Map an address-hex to a worker index by its leading byte, so the partition is
// deterministic and roughly even across K.
function shardOf(addrHex, k) {
  const h = addrHex.startsWith('0x') ? addrHex.slice(2) : addrHex;
  const b = parseInt(h.slice(0, 2), 16) || 0;
  return b % k;
}

// Fetch `addrs` through one worker with up to `conc` requests in flight, until
// the list is drained or `deadline` (performance.now ms) passes.
async function drainShard(wk, addrs, conc, stats, deadline) {
  let i = 0;
  // Per-fetch timeout so one stuck worker retrieval cannot stall a lane past the
  // window; the chunk just counts as failed and the lane moves on.
  const FETCH_TIMEOUT_MS = 20000;
  async function lane() {
    while (i < addrs.length && performance.now() < deadline) {
      const idx = i++;
      const addr = addrs[idx];
      try {
        const m = await wk.call({ type: 'fetch', address: addr }, null, FETCH_TIMEOUT_MS);
        stats.bytes += m.bytes.length;
        stats.chunks += 1;
      } catch (e) {
        stats.failed += 1;
      }
    }
  }
  const lanes = Math.min(conc, Math.max(1, addrs.length));
  await Promise.all(Array.from({ length: lanes }, lane));
}

// Classify a batch of references as plain (downloadable) or erasure-coded.
//
// Boots ONE headless worker node, waits a warmup for it to bootstrap a peer set,
// then for each reference resolves its file root and reads the joiner span via
// `fileSize`. A plain file's span equals its true byte length; an erasure-coded
// file's root chunk carries a redundancy-level marker in the span's top byte, so
// the joiner reports a level-polluted exabyte-scale value. The caller passes the
// expected CSV size per ref; we tag plain when the reported size matches it and
// erasure when it is an exabyte-scale garbage value (top span byte set).
//
// `refs` is an array of { ref, sizeBytes }. Returns one row per ref with the raw
// fileSize the joiner reported, so the caller can audit the top span byte.
window.__classifyRefs = async function (refs, warmupMs, perCallTimeoutMs) {
  warmupMs = warmupMs || 16000;
  perCallTimeoutMs = perCallTimeoutMs || 120000;
  const t0 = performance.now();
  const { workers, overlays } = await bootWorkers(1);
  const wk = workers[0];

  await new Promise((r) => setTimeout(r, warmupMs));
  const tWarm = performance.now();

  const rows = [];
  for (const item of refs) {
    const ref = item.ref;
    const sizeBytes = item.sizeBytes;
    const row = { ref, sizeBytes };
    try {
      const rootMsg = await wk.call({ type: 'resolveRoot', address: ref }, null, perCallTimeoutMs);
      row.fileRoot = rootMsg.fileRoot;
      const sizeMsg = await wk.call({ type: 'size', fileRoot: row.fileRoot }, null, perCallTimeoutMs);
      // `size` is an f64 carrying a u64; an erasure root yields an exabyte-scale
      // value (top span byte set), a plain root yields the exact byte length.
      row.fileSize = sizeMsg.size;
      // Plain when the joiner span matches the CSV byte length exactly. Erasure
      // when the span is exabyte-scale garbage (>> any real ZIM, top byte set).
      const matches = Number.isFinite(sizeBytes) && Math.abs(row.fileSize - sizeBytes) < 1;
      const garbage = row.fileSize > 1e15; // ~petabyte+, far past any real file
      row.classification = matches ? 'plain' : (garbage ? 'erasure' : 'unknown');
    } catch (e) {
      row.classification = 'error';
      row.error = String(e && e.message ? e.message : e);
    }
    rows.push(row);
    console.log('CLASSIFY-ROW ' + JSON.stringify(row));
  }

  for (const w of workers) w.w.terminate();
  return {
    overlays,
    warmupSecs: (tWarm - t0) / 1000,
    rows,
    plain: rows.filter((r) => r.classification === 'plain').length,
    erasure: rows.filter((r) => r.classification === 'erasure').length,
    other: rows.filter((r) => r.classification !== 'plain' && r.classification !== 'erasure').length,
  };
};

// Run a K-worker byte-range sharded download of `ref`.
//
// Worker 0 resolves the file root and its total size once. The file is split
// into K contiguous byte ranges; worker k downloads range k via the efficient
// range-prefetch path (the same wide concurrent prefetch the monolithic node
// uses, scoped to the range), then transfers its whole ordered slice back in one
// ArrayBuffer (no per-chunk postMessage). The coordinator reassembles by byte
// offset and verifies byte-completion.
//
// Each worker runs the monolithic ~150-190 KB/s pipeline on its own slice, so K
// workers aggregate toward K x the single-node rate, bounded by the per-worker
// WebSocket budget (per worker, not per origin).
window.__shardRangeDownload = async function (ref, k, warmupMs, runTimeoutMs, width, path, footprint, bootstrap) {
  k = k || 4;
  warmupMs = warmupMs || 18000;
  runTimeoutMs = runTimeoutMs || 240000;
  width = width || 0;
  // Measured operating point on the live network (hoverfly 3.6 MB wasm): a small
  // per-worker connection footprint (total dial target ~32, bootstrap fill ~12)
  // cuts each worker's dial/connection churn and lifts aggregate throughput about
  // 48% over the full default footprint, while four workers still fill the
  // renderer's socket pool. Larger K thrashes the pool; a larger footprint does
  // not help. Callers may override both.
  footprint = footprint || 32;
  bootstrap = bootstrap || 12;
  const t0 = performance.now();
  // Byte-range workers cannot address-bias (a byte slice scatters across the
  // address space), so only the footprint knob applies here.
  const { workers, overlays } = await bootWorkers(k, { footprint, bootstrap });

  // One warmup window for all K workers in parallel (they bootstrap concurrently).
  await new Promise((r) => setTimeout(r, warmupMs));
  const tWarm = performance.now();

  // Resolve the file root and total size on worker 0. A `path` selects one entry
  // of a multi-file manifest; otherwise the single-file manifest pick is used.
  const rootMsg = path
    ? await workers[0].call({ type: 'resolvePath', address: ref, path }, null, 120000)
    : await workers[0].call({ type: 'resolveRoot', address: ref }, null, 120000);
  const fileRoot = rootMsg.fileRoot;
  const sizeMsg = await workers[0].call({ type: 'size', fileRoot }, null, 120000);
  const total = sizeMsg.size;
  const tRoot = performance.now();
  if (!total) throw new Error('could not resolve file size for range split');

  // Contiguous byte ranges, one per worker; last range absorbs the remainder.
  const ranges = [];
  const base = Math.floor(total / k);
  for (let i = 0; i < k; i++) {
    const offset = i * base;
    const len = (i === k - 1) ? (total - offset) : base;
    ranges.push({ offset, len });
  }

  // Assemble into one contiguous buffer so the file can be byte-verified
  // (wasm magic + SHA-256) against the gateway, proving the sharded download is
  // not just length-complete but bit-correct.
  const file = new Uint8Array(total);
  const tFetch0 = performance.now();
  const perWorker = new Array(k).fill(null);
  const slices = new Array(k).fill(null);
  await Promise.all(workers.map(async (wk, i) => {
    const r = ranges[i];
    const w0 = performance.now();
    const m = await wk.call({ type: 'range', fileRoot, offset: r.offset, len: r.len, width }, null, runTimeoutMs);
    const w1 = performance.now();
    slices[i] = { offset: r.offset, len: m.bytes.length };
    file.set(m.bytes.subarray(0, Math.min(m.bytes.length, total - r.offset)), r.offset);
    const secs = (w1 - w0) / 1000;
    perWorker[i] = {
      offset: r.offset,
      expected: r.len,
      got: m.bytes.length,
      secs: Number(secs.toFixed(2)),
      kbps: Number(((m.bytes.length / 1024) / secs).toFixed(2)),
    };
  }));
  const tEnd = performance.now();

  // Reassemble by byte offset; verify every range came back at its full length.
  let assembled = 0;
  let complete = true;
  for (let i = 0; i < k; i++) {
    assembled += slices[i].len;
    if (slices[i].len !== ranges[i].len) complete = false;
  }

  // Correctness proof: leading magic bytes plus a SHA-256 over the whole file.
  const magic = Array.from(file.subarray(0, 4)).map((b) => b.toString(16).padStart(2, '0')).join('');
  let sha256 = null;
  try {
    const digest = await crypto.subtle.digest('SHA-256', file);
    sha256 = Array.from(new Uint8Array(digest)).map((b) => b.toString(16).padStart(2, '0')).join('');
  } catch (e) { sha256 = 'sha-error:' + String(e && e.message ? e.message : e); }

  for (const wk of workers) wk.w.terminate();

  const fetchSecs = (tEnd - tFetch0) / 1000;
  return {
    mode: 'range',
    k, warmupMs, width,
    overlays,
    fileRoot,
    total,
    assembled,
    byteComplete: complete && assembled === total,
    magic,
    wasmMagic: magic === '0061736d',
    sha256,
    perWorker,
    warmupSecs: ((tWarm - t0) / 1000),
    rootSecs: ((tRoot - tWarm) / 1000),
    fetchSecs: Number(fetchSecs.toFixed(2)),
    kbps: Number(((assembled / 1024) / fetchSecs).toFixed(2)),
    mbps: Number(((assembled / 1048576) / fetchSecs).toFixed(3)),
  };
};

// Run a K-worker ADDRESS-SHARDED download of `ref`.
//
// Each worker's overlay is biased into one slice of the address space
// (prefixBits = log2(K)) and runs a small connection footprint. Worker 0
// enumerates the file's leaves in tree order, each tagged with its byte offset.
// Leaves are partitioned by their leading address bits to the worker whose slice
// they fall in, so the closest peer to each leaf is in that worker's connected
// set (the not-connected retrieval tax collapses). Each worker fetches only its
// slice's leaves and returns them as (offset, bytes); the coordinator writes each
// at its offset and byte-verifies the whole file (wasm magic + SHA-256).
//
// K must be a power of two so prefixBits = log2(K) partitions the byte cleanly.
window.__shardAddrDownload = async function (ref, k, warmupMs, runTimeoutMs, width, path, footprint, bootstrap) {
  k = k || 4;
  warmupMs = warmupMs || 16000;
  runTimeoutMs = runTimeoutMs || 240000;
  width = width || 0;
  footprint = footprint || 0;
  bootstrap = bootstrap || 0;
  const prefixBits = Math.round(Math.log2(k));
  if ((1 << prefixBits) !== k) throw new Error('K must be a power of two for address sharding');

  const t0 = performance.now();
  const { workers, overlays } = await bootWorkers(k, { prefixBits, footprint, bootstrap });
  await new Promise((r) => setTimeout(r, warmupMs));
  const tWarm = performance.now();

  // Resolve file root + size on worker 0 (any worker can resolve; its slice bias
  // does not stop it walking the manifest/intermediates).
  const rootMsg = path
    ? await workers[0].call({ type: 'resolvePath', address: ref, path }, null, 120000)
    : await workers[0].call({ type: 'resolveRoot', address: ref }, null, 120000);
  const fileRoot = rootMsg.fileRoot;
  const sizeMsg = await workers[0].call({ type: 'size', fileRoot }, null, 120000);
  const total = sizeMsg.size;
  if (!total) throw new Error('could not resolve file size');

  // Enumerate leaves in tree order with byte offsets on worker 0.
  const leavesMsg = await workers[0].call({ type: 'listLeaves', fileRoot }, null, 180000);
  const flat = leavesMsg.leaves || [];
  const leaves = [];
  for (let i = 0; i + 1 < flat.length; i += 2) leaves.push({ addr: flat[i], offset: flat[i + 1] });
  const tList = performance.now();
  if (!leaves.length) throw new Error('no leaves enumerated');

  // Partition leaves by address slice.
  const shards = Array.from({ length: k }, () => []);
  for (const l of leaves) shards[workerForAddr(l.addr, prefixBits)].push(l);

  // Each worker fetches its slice; returns one concatenated body buffer with
  // parallel offsets/lengths. The coordinator writes each body at its offset.
  const file = new Uint8Array(total);
  let assembled = 0;
  const tFetch0 = performance.now();
  const perWorker = new Array(k).fill(null);
  await Promise.all(workers.map(async (wk, i) => {
    const pairs = [];
    for (const l of shards[i]) { pairs.push(l.addr); pairs.push(l.offset); }
    const w0 = performance.now();
    const m = await wk.call({ type: 'fetchLeaves', pairs, width }, null, runTimeoutMs);
    const w1 = performance.now();
    const offsets = m.offsets, lengths = m.lengths, bytes = m.bytes;
    let pos = 0, wbytes = 0;
    for (let j = 0; j < offsets.length; j++) {
      const off = offsets[j], len = lengths[j];
      file.set(bytes.subarray(pos, pos + len), off);
      pos += len; wbytes += len;
    }
    assembled += wbytes;
    const secs = (w1 - w0) / 1000;
    perWorker[i] = {
      leaves: shards[i].length,
      fetched: offsets.length,
      bytes: wbytes,
      secs: Number(secs.toFixed(2)),
      kbps: Number(((wbytes / 1024) / secs).toFixed(2)),
    };
  }));
  const tEnd = performance.now();

  const magic = Array.from(file.subarray(0, 4)).map((b) => b.toString(16).padStart(2, '0')).join('');
  let sha256 = null;
  try {
    const digest = await crypto.subtle.digest('SHA-256', file);
    sha256 = Array.from(new Uint8Array(digest)).map((b) => b.toString(16).padStart(2, '0')).join('');
  } catch (e) { sha256 = 'sha-error:' + String(e && e.message ? e.message : e); }

  for (const wk of workers) wk.w.terminate();

  const fetchSecs = (tEnd - tFetch0) / 1000;
  return {
    mode: 'addr',
    k, prefixBits, warmupMs, width, footprint, bootstrap,
    overlays,
    fileRoot,
    total,
    assembled,
    byteComplete: assembled === total,
    magic,
    wasmMagic: magic === '0061736d',
    sha256,
    leafCount: leaves.length,
    shardSizes: shards.map((s) => s.length),
    perWorker,
    warmupSecs: ((tWarm - t0) / 1000),
    listSecs: ((tList - tWarm) / 1000),
    fetchSecs: Number(fetchSecs.toFixed(2)),
    kbps: Number(((assembled / 1024) / fetchSecs).toFixed(2)),
    mbps: Number(((assembled / 1048576) / fetchSecs).toFixed(3)),
  };
};

// Run a K-worker sharded fetch of `ref`. `conc` is per-worker in-flight lanes,
// `warmupMs` is the per-worker node warmup before enumeration/fetch begins.
window.__shardDownload = async function (ref, k, conc, warmupMs, fetchWindowMs, presetAddrs) {
  k = k || 3;
  conc = conc || 64;
  warmupMs = warmupMs || 14000;
  fetchWindowMs = fetchWindowMs || 60000;
  const t0 = performance.now();
  const { workers, overlays } = await bootWorkers(k);

  // Give each worker's node a warmup to bootstrap a peer set before fetching,
  // so its forwarding has somewhere to route. The fetch path itself retries
  // (busy/wave logic) so this only needs to clear the cold-start.
  await new Promise((r) => setTimeout(r, warmupMs));
  const tWarm = performance.now();

  // Enumerate the chunk tree on worker 0 (intermediates only), unless a static
  // address list is supplied. Listing on a cold worker is slow; a preset list
  // isolates the fetch-throughput measurement (the K-scaling signal) from the
  // one-time enumeration cost.
  let all;
  if (Array.isArray(presetAddrs) && presetAddrs.length) {
    all = presetAddrs;
  } else {
    const listMsg = await workers[0].call({ type: 'list', address: ref, max: 6000 }, null, 180000)
      .catch((e) => ({ addrs: [], listError: String(e.message || e) }));
    all = listMsg.addrs || [];
    if (all.length) console.log('SHARD-ADDRS ' + JSON.stringify(all));
  }
  const tList = performance.now();

  // Partition by address across the K workers.
  const shards = Array.from({ length: k }, () => []);
  for (const a of all) shards[shardOf(a, k)].push(a);

  // Drain all shards in parallel; each worker fetches its own shard until the
  // list is exhausted or the measurement window closes.
  const stats = { bytes: 0, chunks: 0, failed: 0 };
  const tFetch0 = performance.now();
  const deadline = tFetch0 + fetchWindowMs;
  await Promise.all(workers.map((wk, i) => drainShard(wk, shards[i], conc, stats, deadline)));
  const tEnd = performance.now();

  for (const wk of workers) wk.w.terminate();

  const fetchSecs = (tEnd - tFetch0) / 1000;
  return {
    k, conc, warmupMs,
    overlays,
    totalChunks: all.length,
    fetched: stats.chunks,
    failed: stats.failed,
    bytes: stats.bytes,
    shardSizes: shards.map((s) => s.length),
    warmupSecs: ((tWarm - t0) / 1000),
    listSecs: ((tList - tWarm) / 1000),
    fetchSecs,
    kbps: Number(((stats.bytes / 1024) / fetchSecs).toFixed(2)),
    chunksPerSec: Number((stats.chunks / fetchSecs).toFixed(2)),
  };
};
