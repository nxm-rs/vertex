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

async function bootWorkers(k) {
  const workers = [];
  for (let i = 0; i < k; i++) workers.push(makeWorker());
  const readies = await Promise.all(workers.map((wk) => wk.call({ type: 'boot' })));
  return { workers, overlays: readies.map((r) => r.overlay) };
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
window.__shardRangeDownload = async function (ref, k, warmupMs, runTimeoutMs, width) {
  k = k || 3;
  warmupMs = warmupMs || 14000;
  runTimeoutMs = runTimeoutMs || 240000;
  width = width || 0;
  const t0 = performance.now();
  const { workers, overlays } = await bootWorkers(k);

  // One warmup window for all K workers in parallel (they bootstrap concurrently).
  await new Promise((r) => setTimeout(r, warmupMs));
  const tWarm = performance.now();

  // Resolve the file root and total size on worker 0.
  const rootMsg = await workers[0].call({ type: 'resolveRoot', address: ref }, null, 120000);
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

  const tFetch0 = performance.now();
  const perWorker = new Array(k).fill(null);
  const slices = new Array(k).fill(null);
  await Promise.all(workers.map(async (wk, i) => {
    const r = ranges[i];
    const w0 = performance.now();
    const m = await wk.call({ type: 'range', fileRoot, offset: r.offset, len: r.len, width }, null, runTimeoutMs);
    const w1 = performance.now();
    slices[i] = { offset: r.offset, len: m.bytes.length };
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
    perWorker,
    warmupSecs: ((tWarm - t0) / 1000),
    rootSecs: ((tRoot - tWarm) / 1000),
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
