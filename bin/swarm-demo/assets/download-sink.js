// Browser download sink factory.
//
// `createDownloadSink(filename, sizeHintOrNull)` returns a sink the Rust wasm
// streaming download writes to:
//
//   { seekable, setTotal(n), close(): Promise, abort(reason),
//     write(Uint8Array): Promise,                                  // ordered
//     setLen(n): Promise, writeAt(offset, Uint8Array): Promise }   // seekable
//
// Three implementations, feature-detected at call time. `seekable` tells the
// Rust side whether it may deliver leaves out of order (positional writes) or
// must stream in file order.
//
//   * OPFS worker (Chromium/Firefox/Safari, preferred): a dedicated worker owns
//     an Origin Private File System scratch file via a sync access handle and
//     writes each leaf at its offset off the fetch thread; on close the finished
//     file is delivered to Downloads as a disk-backed Blob. `seekable`. Needs no
//     user gesture, so it is tried first.
//
//   * File System Access (Chromium/Edge): `showSaveFilePicker` opens the native
//     save dialog and streams straight to a `FileSystemWritableFileStream`. The
//     picker REQUIRES a user gesture, so this factory must be called inside the
//     download click handler, before any awaiting. Ordered.
//
//   * Service worker (Firefox/Safari): a StreamSaver-style worker answers a
//     navigation to a synthetic URL with a streamed attachment Response fed from
//     a MessagePort. Backpressure flows back over the port as `pull` signals.
//     Ordered.
//
// Keeping all browser-specific plumbing here lets the Rust side stay a plain
// `extern` binding with no unstable cfgs.

const SW_URL = resolveRootUrl('download-sw.js');
const WORKER_URL = resolveRootUrl('download-worker.js');
const DL_PREFIX = '__stream_dl__';
// Prefix for the OPFS scratch files the worker writes to before delivery.
const SCRATCH_PREFIX = '__swarm_dl__';

// Resolve a path relative to the demo root (one level up from assets/), so the
// worker is registered at root scope and can control the synthetic download URL.
function resolveRootUrl(rel) {
  const assetsBase = new URL('.', import.meta.url); // .../assets/
  return new URL('../' + rel, assetsBase).href;
}

let swRegistration = null;
async function ensureServiceWorker() {
  if (!('serviceWorker' in navigator)) {
    return null;
  }
  if (swRegistration) {
    return swRegistration;
  }
  const scope = resolveRootUrl('');
  swRegistration = await navigator.serviceWorker.register(SW_URL, { scope });
  await navigator.serviceWorker.ready;
  return swRegistration;
}

// Pre-register the worker eagerly so the first download does not pay for it.
ensureServiceWorker().catch(() => {});

function randomId() {
  return Math.random().toString(36).slice(2) + Date.now().toString(36);
}

// File System Access sink: stream to a picked file. Must be created in-gesture.
class FsaSink {
  constructor(writable) {
    this.seekable = false;
    this.writable = writable;
    this.written = 0;
    this.total = null;
  }
  setTotal(n) {
    this.total = n;
    notifyProgress(this.written, this.total);
  }
  async write(chunk) {
    await this.writable.write(chunk);
    this.written += chunk.length;
    notifyProgress(this.written, this.total);
  }
  async close() {
    await this.writable.close();
  }
  abort(reason) {
    try {
      this.writable.abort(reason);
    } catch (_) {}
  }
}

// Service worker sink: post ordered chunks to the worker over a MessagePort and
// pace sends against the stream's pull signal for backpressure.
class SwSink {
  constructor(id, port) {
    this.seekable = false;
    this.id = id;
    this.port = port;
    this.written = 0;
    this.total = null;
    this.pullWaiters = [];
    this.pulls = 0; // available pull credits
    this.closed = false;
    this.port.onmessage = (ev) => {
      const msg = ev.data;
      if (msg && (msg.type === 'pull')) {
        const waiter = this.pullWaiters.shift();
        if (waiter) {
          waiter();
        } else {
          this.pulls += 1;
        }
      } else if (msg && (msg.type === 'cancel')) {
        this.closed = true;
        for (const w of this.pullWaiters.splice(0)) {
          w();
        }
      }
    };
  }
  setTotal(n) {
    this.total = n;
    notifyProgress(this.written, this.total);
  }
  awaitPull() {
    if (this.closed || this.pulls > 0) {
      if (this.pulls > 0) {
        this.pulls -= 1;
      }
      return Promise.resolve();
    }
    return new Promise((resolve) => this.pullWaiters.push(resolve));
  }
  async write(chunk) {
    // Wait for the consumer to ask before sending, so peak buffering is one
    // chunk in flight rather than the whole file.
    await this.awaitPull();
    // Copy out of wasm memory into a transferable buffer.
    const buf = chunk.slice().buffer;
    this.port.postMessage(buf, [buf]);
    this.written += chunk.length;
    notifyProgress(this.written, this.total);
  }
  async close() {
    this.port.postMessage({ type: 'end' });
    this.closed = true;
  }
  abort(reason) {
    try {
      this.port.postMessage({ type: 'abort' });
    } catch (_) {}
    this.closed = true;
  }
}

// OPFS sync access handles live only in a dedicated worker, so OPFS support
// plus Worker support is the signal for the seekable path.
function opfsSupported() {
  return (
    typeof navigator !== 'undefined' &&
    navigator.storage &&
    typeof navigator.storage.getDirectory === 'function' &&
    typeof Worker !== 'undefined' &&
    typeof document !== 'undefined'
  );
}

// Remove scratch files left by earlier downloads, never the current one. The
// just-finished file is left in place (it is the anchor download's source) and
// swept on the next download instead, so a remove never races the browser
// reading the delivered Blob.
async function sweepOldScratch(keepName) {
  try {
    const root = await navigator.storage.getDirectory();
    const stale = [];
    for await (const [name, handle] of root.entries()) {
      if (name.startsWith(SCRATCH_PREFIX) && name !== keepName && handle.kind === 'file') {
        stale.push(name);
      }
    }
    for (const name of stale) {
      try {
        await root.removeEntry(name);
      } catch (_) {}
    }
  } catch (_) {}
}

// OPFS-worker sink: out-of-order positional writes to a scratch file owned by a
// dedicated worker, then delivery to Downloads as a disk-backed Blob. One write
// is in flight at a time (the Rust side awaits each `writeAt`), so the worker's
// queue and peak memory stay bounded.
class OpfsWorkerSink {
  constructor(worker, scratchName, filename) {
    this.seekable = true;
    this.worker = worker;
    this.scratchName = scratchName;
    this.filename = filename || 'swarm-download.bin';
    this.written = 0;
    this.total = null;
    this.seq = 0;
    this.pending = new Map(); // seq -> { resolve, reject }
    this.lifecycle = null; // { resolve, reject } for opened/closed/aborted
    worker.onmessage = (ev) => this._onMessage(ev.data);
    worker.onerror = (e) =>
      this._failAll(new Error('download worker error: ' + ((e && e.message) || e)));
  }
  _onMessage(msg) {
    if (!msg) return;
    if (msg.type === 'opened' || msg.type === 'closed' || msg.type === 'aborted') {
      const w = this.lifecycle;
      this.lifecycle = null;
      if (w) w.resolve();
      return;
    }
    if (msg.type === 'ack') {
      const p = this.pending.get(msg.seq);
      if (p) {
        this.pending.delete(msg.seq);
        p.resolve();
      }
      return;
    }
    if (msg.type === 'error') {
      const err = new Error(msg.message || 'download worker error');
      if (msg.seq != null && this.pending.has(msg.seq)) {
        const p = this.pending.get(msg.seq);
        this.pending.delete(msg.seq);
        p.reject(err);
      } else {
        this._failAll(err);
      }
    }
  }
  _failAll(err) {
    if (this.lifecycle) {
      const w = this.lifecycle;
      this.lifecycle = null;
      w.reject(err);
    }
    for (const [, p] of this.pending) p.reject(err);
    this.pending.clear();
  }
  _lifecycle() {
    return new Promise((resolve, reject) => {
      this.lifecycle = { resolve, reject };
    });
  }
  _request(type, payload, transfer) {
    const seq = ++this.seq;
    return new Promise((resolve, reject) => {
      this.pending.set(seq, { resolve, reject });
      this.worker.postMessage({ type, seq, ...payload }, transfer || []);
    });
  }
  setTotal(n) {
    this.total = n;
    notifyProgress(this.written, this.total);
  }
  async setLen(len) {
    // Fail clearly before writing if the origin quota cannot hold the file,
    // rather than surfacing a mid-stream ENOSPC.
    try {
      if (navigator.storage && typeof navigator.storage.estimate === 'function') {
        const { quota, usage } = await navigator.storage.estimate();
        if (quota != null && usage != null && quota - usage < len) {
          throw new Error(
            'insufficient browser storage for this download (' +
              len +
              ' bytes needed, ' +
              (quota - usage) +
              ' available)',
          );
        }
      }
    } catch (e) {
      // Re-throw a real quota shortfall; ignore an estimate that simply failed.
      if (e instanceof Error && e.message.startsWith('insufficient browser storage')) throw e;
    }
    await this._request('truncate', { len });
  }
  async writeAt(offset, chunk) {
    // Copy out of wasm memory into a transferable buffer so the worker owns it.
    const buf = chunk.slice().buffer;
    await this._request('write', { offset, buffer: buf }, [buf]);
    this.written += chunk.length;
    notifyProgress(this.written, this.total);
  }
  // Ordered interface, unused on the seekable path but kept for parity.
  async write(chunk) {
    await this.writeAt(this.written, chunk);
  }
  async close() {
    const closed = this._lifecycle();
    this.worker.postMessage({ type: 'close' });
    await closed;
    await this._deliver();
    try {
      this.worker.terminate();
    } catch (_) {}
  }
  abort(reason) {
    try {
      this.worker.postMessage({ type: 'abort' });
    } catch (_) {}
    try {
      this.worker.terminate();
    } catch (_) {}
  }
  // Read the finished scratch file (its sync handle is now closed) and hand it
  // to the browser as a download. The File is a disk-backed snapshot, so this
  // never loads the whole file into memory.
  async _deliver() {
    const root = await navigator.storage.getDirectory();
    const handle = await root.getFileHandle(this.scratchName);
    const file = await handle.getFile();
    const url = URL.createObjectURL(file);
    const a = document.createElement('a');
    a.href = url;
    a.download = this.filename;
    a.rel = 'noopener';
    document.body.appendChild(a);
    a.click();
    a.remove();
    setTimeout(() => {
      try {
        URL.revokeObjectURL(url);
      } catch (_) {}
    }, 120000);
  }
}

// Progress is surfaced through a window-level hook the UI installs; absent that
// it is a no-op, so the sink stays usable from a bare page.
function notifyProgress(written, total) {
  if (typeof window !== 'undefined' && typeof window.__swarmDownloadProgress === 'function') {
    window.__swarmDownloadProgress(written, total);
  }
}

export async function createDownloadSink(filename, sizeHint) {
  // PREFERRED: OPFS worker sink. Seekable, off the fetch thread, and needs no
  // user gesture, so it is safe to try first even though it awaits. A present
  // but unusable OPFS falls through to the service worker, which also needs no
  // gesture; the File System Access picker is only reached when OPFS was never
  // attempted, so its gesture is intact.
  if (opfsSupported()) {
    try {
      const scratchName = SCRATCH_PREFIX + randomId();
      await sweepOldScratch(scratchName);
      const worker = new Worker(WORKER_URL);
      const sink = new OpfsWorkerSink(worker, scratchName, filename);
      const opened = sink._lifecycle();
      worker.postMessage({ type: 'open', name: scratchName });
      await opened;
      if (sizeHint != null && !Number.isNaN(sizeHint)) {
        sink.setTotal(sizeHint);
      }
      return sink;
    } catch (e) {
      console.warn('[download] OPFS sink unavailable, falling back', e);
      return createSwSink(filename, sizeHint);
    }
  }

  // FAST PATH: File System Access. Call the picker synchronously here (still in
  // the gesture), then await the writable.
  if (typeof window !== 'undefined' && typeof window.showSaveFilePicker === 'function') {
    const handle = await window.showSaveFilePicker({
      suggestedName: filename || 'swarm-download.bin',
    });
    const writable = await handle.createWritable();
    const sink = new FsaSink(writable);
    if (sizeHint != null) {
      sink.setTotal(sizeHint);
    }
    return sink;
  }

  return createSwSink(filename, sizeHint);
}

// Service worker stream sink. Register (idempotent), open a port, tell the
// worker about this download, then navigate a hidden iframe to the synthetic URL
// so the browser treats the streamed Response as a download.
async function createSwSink(filename, sizeHint) {
  const reg = await ensureServiceWorker();
  if (!reg) {
    throw new Error('no streaming download path: missing File System Access and service workers');
  }
  const active = reg.active || navigator.serviceWorker.controller;
  if (!active) {
    // The freshly registered worker may not control the page yet on first load.
    await navigator.serviceWorker.ready;
  }
  const worker = reg.active || navigator.serviceWorker.controller;
  if (!worker) {
    throw new Error('service worker did not activate');
  }

  const id = randomId();
  const channel = new MessageChannel();
  const sink = new SwSink(id, channel.port1);
  worker.postMessage(
    {
      type: 'register',
      id,
      filename: filename || 'swarm-download.bin',
      total: sizeHint != null ? sizeHint : null,
      port: channel.port2,
    },
    [channel.port2],
  );

  // Trigger the download navigation via a hidden iframe so the page itself is
  // not navigated away. The worker answers it with the attachment Response.
  const dlUrl = resolveRootUrl(DL_PREFIX + '/' + id);
  const iframe = document.createElement('iframe');
  iframe.hidden = true;
  iframe.src = dlUrl;
  document.body.appendChild(iframe);
  sink._iframe = iframe;
  const origClose = sink.close.bind(sink);
  sink.close = async () => {
    await origClose();
    // Leave the iframe a moment for the browser to commit the download, then
    // drop it.
    setTimeout(() => {
      try {
        iframe.remove();
      } catch (_) {}
    }, 2000);
  };

  if (sizeHint != null) {
    sink.setTotal(sizeHint);
  }
  return sink;
}

// Expose for the wasm glue and for tests/manual use.
if (typeof window !== 'undefined') {
  window.createDownloadSink = createDownloadSink;
}
