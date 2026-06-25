// Worker entry: boots a headless Swarm client node in this Web Worker, then
// answers fetch-chunk requests over postMessage. One node per worker, each with
// its own ephemeral identity and peer set, so K workers give K independent
// forwarding pipelines that do not share a per-peer in-flight budget.
//
// Loaded as a module worker (`new Worker(url, { type: 'module' })`) so it can
// `import` the wasm-bindgen glue and `await init(...)` the wasm, both of which a
// WorkerGlobalScope supports.
import init, { startWorkerNode } from '/swarm-demo.js';

let node = null;

// A RandomAccessSink-shaped object backed by a MessagePort to the shared OPFS
// sink worker. The wasm streaming download calls `setTotal/writeAt/close/abort`
// on it; `writeAt` posts each (offset, buffer) to the sink worker and awaits its
// ack (backpressure), so this fetch worker streams its slice with no whole-range
// buffer. The buffer is transferred zero-copy; the next write waits for the prior
// disk write, so at most one write is outstanding per worker.
//
// `close` is a no-op: the OPFS handle is owned by the sink worker and closed once
// by the coordinator after ALL fetch workers finish, not per worker. `setTotal`
// is a no-op too: progress is driven from the sink worker's aggregate
// `bytesWritten`, not per-slice totals.
function makePortSink(port) {
  let pendingAck = null;
  port.onmessage = (ev) => {
    const msg = ev.data;
    if (!msg) return;
    const waiter = pendingAck;
    pendingAck = null;
    if (!waiter) return;
    if (msg.type === 'ack') waiter.resolve(msg.bytesWritten);
    else if (msg.type === 'error') waiter.reject(new Error(msg.error));
  };
  return {
    setTotal(_n) {},
    writeAt(offset, data) {
      // `data` is a Uint8Array over a fresh ArrayBuffer (the wasm side copies out
      // before calling), so transfer its buffer zero-copy to the sink worker.
      const buffer = data.buffer;
      return new Promise((resolve, reject) => {
        pendingAck = { resolve, reject };
        port.postMessage({ type: 'write', offset, buffer }, [buffer]);
      });
    },
    close() {
      return Promise.resolve();
    },
    abort(_reason) {},
  };
}

self.onmessage = async (e) => {
  const msg = e.data || {};
  try {
    if (msg.type === 'boot') {
      await init({ module_or_path: '/swarm-demo_bg.wasm' });
      // Optional per-worker address bias and footprint: prefixBits/prefixValue
      // steer this node's overlay into its assigned address slice, footprint/
      // bootstrap shrink its connection budget so K workers fit under the pool.
      const prefixBits = msg.prefixBits | 0;
      const prefixValue = msg.prefixValue | 0;
      const footprint = msg.footprint | 0;
      const bootstrap = msg.bootstrap | 0;
      node = await startWorkerNode(prefixBits, prefixValue, footprint, bootstrap);
      self.postMessage({ type: 'ready', id: msg.id, overlay: node.overlay });
      return;
    }
    if (msg.type === 'list') {
      if (!node) throw new Error('node not booted');
      const addrs = await node.listChunks(msg.address, msg.max || 0);
      self.postMessage({ type: 'list', id: msg.id, addrs: Array.from(addrs) });
      return;
    }
    if (msg.type === 'fetch') {
      if (!node) throw new Error('node not booted');
      const bytes = await node.fetchChunk(msg.address);
      // Transfer the ArrayBuffer so the bytes are moved, not copied, to main.
      const buf = bytes.buffer;
      self.postMessage({ type: 'chunk', id: msg.id, address: msg.address, bytes }, [buf]);
      return;
    }
    if (msg.type === 'resolveRoot') {
      if (!node) throw new Error('node not booted');
      const fileRoot = await node.resolveFileRoot(msg.address);
      self.postMessage({ type: 'resolveRoot', id: msg.id, fileRoot });
      return;
    }
    if (msg.type === 'resolvePath') {
      if (!node) throw new Error('node not booted');
      const fileRoot = await node.resolveFilePath(msg.address, msg.path);
      self.postMessage({ type: 'resolvePath', id: msg.id, fileRoot });
      return;
    }
    if (msg.type === 'size') {
      if (!node) throw new Error('node not booted');
      const size = await node.fileSize(msg.fileRoot);
      self.postMessage({ type: 'size', id: msg.id, size });
      return;
    }
    if (msg.type === 'listLeaves') {
      if (!node) throw new Error('node not booted');
      const leaves = await node.listLeaves(msg.fileRoot);
      self.postMessage({ type: 'listLeaves', id: msg.id, leaves: Array.from(leaves) });
      return;
    }
    if (msg.type === 'fetchLeaves') {
      if (!node) throw new Error('node not booted');
      // `pairs` is a flat [addrHex, offset, ...]. The result is one concatenated
      // body buffer plus parallel offsets/lengths; transfer the buffer to main.
      const res = await node.fetchLeavesAt(msg.pairs, msg.width || 0);
      const bytes = res.bytes;
      self.postMessage({
        type: 'fetchLeaves', id: msg.id,
        offsets: res.offsets, lengths: res.lengths, bytes,
      }, [bytes.buffer]);
      return;
    }
    if (msg.type === 'streamRange') {
      if (!node) throw new Error('node not booted');
      // Stream this worker's byte slice CHUNK-GRANULARLY into the shared sink
      // worker over the transferred MessagePort: each decoded leaf posts straight
      // to the sink at its ABSOLUTE file offset, no whole-range buffer here and no
      // main-thread relay. Resolves once the whole slice is on disk.
      const sink = makePortSink(msg.port);
      await node.streamRangeToSink(msg.fileRoot, msg.offset, msg.len, msg.width || 0, sink);
      self.postMessage({ type: 'streamRange', id: msg.id, offset: msg.offset, len: msg.len });
      return;
    }
    if (msg.type === 'range') {
      if (!node) throw new Error('node not booted');
      // Download this worker's byte slice via the efficient range path, then
      // transfer the whole slice to main in one large ArrayBuffer (no per-chunk
      // postMessage). The coordinator writes it at `msg.offset`.
      const bytes = await node.downloadRange(msg.fileRoot, msg.offset, msg.len, msg.width || 0);
      const buf = bytes.buffer;
      self.postMessage({ type: 'range', id: msg.id, offset: msg.offset, bytes }, [buf]);
      return;
    }
  } catch (err) {
    self.postMessage({ type: 'error', id: msg.id, address: msg.address, err: String(err && err.message ? err.message : err) });
  }
};
