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

self.onmessage = async (e) => {
  const msg = e.data || {};
  try {
    if (msg.type === 'boot') {
      await init({ module_or_path: '/swarm-demo_bg.wasm' });
      node = await startWorkerNode();
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
    if (msg.type === 'size') {
      if (!node) throw new Error('node not booted');
      const size = await node.fileSize(msg.fileRoot);
      self.postMessage({ type: 'size', id: msg.id, size });
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
