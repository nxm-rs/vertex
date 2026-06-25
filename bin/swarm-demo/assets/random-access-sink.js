// Random-access browser download sink factory.
//
// `createRandomAccessSink(filename, sizeHintOrNull)` returns a sink the wasm
// streaming download writes positionally to, in any order:
//
//   { writeAt(offset, Uint8Array): Promise, setTotal(n), close(): Promise,
//     abort(reason) }
//
// Unlike the ordered `download-sink.js`, writes carry an explicit byte offset
// and may arrive out of order: the stream traverses the chunk tree at full
// concurrency and writes each leaf to its exact position the moment it decodes,
// so a slow chunk never blocks the writes ready behind it.
//
// Implementations, feature-detected at call time:
//
//   * File System Access (Chromium/Edge): `showSaveFilePicker` opens the native
//     save dialog and an async `FileSystemWritableFileStream` streams straight to
//     the picked file via the `{ type: 'write', position, data }` command. The
//     picker REQUIRES a user gesture, so this factory consumes a handle opened
//     in-gesture by `openRandomAccessPicker` (stashed on `window`).
//
//   * OPFS (Firefox/Chromium/headless): the origin-private file system stages
//     the file under a fixed name. The positional writes run by default in
//     `sink-worker.js` on a `SyncAccessHandle` (the fast synchronous write,
//     worker-only) so they leave the main thread, with `?inlinesink` selecting
//     the main-thread `createWritable` baseline for A/B. `getDirectory` needs no
//     gesture, so this path is driveable headlessly: after close the worker
//     releases its exclusive handle and the test re-opens the same OPFS file,
//     reads it back, and hashes it to byte-verify the assembly.
//     The user cannot reach an OPFS file directly, so on `close()` this sink
//     hands the staged file off to the user without re-buffering it: it opens
//     the staged file, takes a `ReadableStream` via `getFile().stream()`, and
//     pipes it to the StreamSaver-style download service worker (`download-sw.js`),
//     pacing sends against the worker's pull signal. Peak retained memory stays
//     one stream chunk, never the whole file, so an 800 MB download does not OOM.
//
// Keeping the browser plumbing here lets the Rust side stay a plain `extern`
// binding with async writeAt/close/abort/setTotal.

// Fixed staging name for the OPFS back-end, so a test can re-open and read back
// the assembled file to byte-verify it.
const OPFS_STAGING_NAME = '__swarm_ra_download__';

// Service-worker download plumbing, shared with the ordered sink: resolve the
// worker at the demo root (one level up from assets/) so it controls the
// synthetic download URL, register it once, and reuse it for OPFS hand-off.
const SW_URL = resolveRootUrl('download-sw.js');
const DL_PREFIX = '__stream_dl__';

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

function randomId() {
  return Math.random().toString(36).slice(2) + Date.now().toString(36);
}

// Pre-register the worker eagerly so the OPFS hand-off does not pay for it.
if (typeof navigator !== 'undefined' && 'serviceWorker' in navigator) {
  ensureServiceWorker().catch(() => {});
}

// Progress is surfaced through the same window-level hook the ordered sink
// uses; absent that it is a no-op, so the sink stays usable from a bare page.
function notifyProgress(written, total) {
  if (typeof window !== 'undefined' && typeof window.__swarmDownloadProgress === 'function') {
    window.__swarmDownloadProgress(written, total);
  }
}

// True when the inline main-thread `createWritable` OPFS path is requested via
// `?inlinesink` (the A/B baseline); otherwise the OPFS path uses the sink worker.
function inlineSinkRequested() {
  if (typeof window === 'undefined' || !window.location) {
    return false;
  }
  try {
    return new URLSearchParams(window.location.search).has('inlinesink');
  } catch (_) {
    return false;
  }
}

// A random-access sink whose positional writes run in `sink-worker.js` on an
// OPFS `SyncAccessHandle`, off the main thread. Each `writeAt` transfers the
// chunk's ArrayBuffer to the worker (zero-copy) and awaits the worker's `ack`,
// so the caller paces to the worker's write rate and the postMessage queue never
// grows past one outstanding write. The handle is exclusive: the main thread
// must not open the staged file until `close()` resolves.
class WorkerOpfsSink {
  constructor(worker, stagingName) {
    this.worker = worker;
    this.stagingName = stagingName;
    this.written = 0;
    this.total = null;
    this.seq = 0;
    // Pending requests keyed by reply type; the worker answers each in order.
    this.pending = new Map();
    this.worker.onmessage = (ev) => {
      const msg = ev.data;
      if (!msg) return;
      if (msg.type === 'error') {
        for (const { reject } of this.pending.values()) {
          reject(new Error(msg.error));
        }
        this.pending.clear();
        return;
      }
      const waiter = this.pending.get(msg.type);
      if (waiter) {
        this.pending.delete(msg.type);
        waiter.resolve(msg);
      }
    };
    this.worker.onerror = (ev) => {
      const err = new Error(ev && ev.message ? ev.message : 'sink worker error');
      for (const { reject } of this.pending.values()) reject(err);
      this.pending.clear();
    };
  }
  _await(replyType) {
    return new Promise((resolve, reject) => {
      this.pending.set(replyType, { resolve, reject });
    });
  }
  setTotal(n) {
    this.total = n;
    notifyProgress(this.written, this.total);
  }
  // Write `data` at `position`. `data` is a Uint8Array over a freshly allocated
  // ArrayBuffer (the wasm side copies out before calling), so its buffer is
  // transferred to the worker zero-copy and detached on this side. Awaiting the
  // `ack` is the backpressure: the next write waits for the prior disk write.
  async writeAt(position, data) {
    const buffer = data.buffer;
    const ack = this._await('ack');
    this.worker.postMessage({ type: 'write', offset: position, buffer }, [buffer]);
    const reply = await ack;
    this.written = reply.bytesWritten;
    notifyProgress(this.written, this.total);
  }
  async close() {
    const closed = this._await('closed');
    this.worker.postMessage({ type: 'close' });
    await closed;
    this.worker.terminate();
  }
  abort(_reason) {
    try {
      this.worker.terminate();
    } catch (_) {}
  }
}

// A random-access sink over an open `FileSystemWritableFileStream`. Writes are
// positional; `written` tracks the byte count for progress (a file may be
// written sparsely, so this is "bytes delivered", not "highest offset").
class WritablePositionSink {
  constructor(writable) {
    this.writable = writable;
    this.written = 0;
    this.total = null;
  }
  setTotal(n) {
    this.total = n;
    notifyProgress(this.written, this.total);
  }
  // Write `data` at byte `position`. The write command form is supported on
  // both FSA and OPFS writables and seeks without truncating the file.
  async writeAt(position, data) {
    await this.writable.write({ type: 'write', position, data });
    this.written += data.length;
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

// Build a sink from a (possibly pending) FSA save handle. Re-throws an
// AbortError verbatim (user cancel), and returns null on any other failure so
// the caller falls back to OPFS.
async function fsaSinkFrom(handlePromise, sizeHint) {
  try {
    const handle = await handlePromise;
    // `keepExistingData` so a positional write past the start does not zero the
    // already-written bytes; the file is sized up front when the total is known.
    const writable = await handle.createWritable({ keepExistingData: true });
    const sink = new WritablePositionSink(writable);
    if (sizeHint != null && Number.isFinite(sizeHint)) {
      sink.setTotal(sizeHint);
    }
    return sink;
  } catch (err) {
    if (err && err.name === 'AbortError') {
      const cancelled = new Error('save cancelled');
      cancelled.name = 'AbortError';
      throw cancelled;
    }
    console.warn('random-access save picker failed, falling back to OPFS', err);
    return null;
  }
}

// Build a sink over an OPFS-staged file under `OPFS_STAGING_NAME`, so a test can
// re-open and read it back. The default back-end runs the positional writes in
// `sink-worker.js` on a `SyncAccessHandle` off the main thread; `?inlinesink`
// selects the main-thread `createWritable` baseline for A/B. On `close()` the
// staged file is delivered to the user via the download service worker, streamed
// (never re-buffered whole), so memory stays bounded. The worker holds an
// exclusive lock until close, so delivery (which opens the file) runs only after.
async function opfsSink(filename, sizeHint) {
  const useInline = inlineSinkRequested();
  let sink;
  if (useInline) {
    const dir = await navigator.storage.getDirectory();
    const handle = await dir.getFileHandle(OPFS_STAGING_NAME, { create: true });
    const writable = await handle.createWritable({ keepExistingData: true });
    sink = new WritablePositionSink(writable);
  } else {
    const worker = new Worker(new URL('sink-worker.js', import.meta.url), { type: 'module' });
    const opened = new Promise((resolve, reject) => {
      worker.onmessage = (ev) => {
        if (ev.data && ev.data.type === 'opened') resolve();
        else if (ev.data && ev.data.type === 'error') reject(new Error(ev.data.error));
      };
      worker.onerror = (ev) => reject(new Error(ev && ev.message ? ev.message : 'sink worker error'));
    });
    worker.postMessage({ type: 'open', filename: OPFS_STAGING_NAME });
    await opened;
    sink = new WorkerOpfsSink(worker, OPFS_STAGING_NAME);
  }
  if (sizeHint != null && Number.isFinite(sizeHint)) {
    sink.setTotal(sizeHint);
  }
  // After the positional writes finish and the sink closes (releasing the OPFS
  // handle), open the staged file and deliver it to the user as a streamed
  // attachment.
  const closeInner = sink.close.bind(sink);
  sink.close = async () => {
    await closeInner();
    const dir = await navigator.storage.getDirectory();
    const handle = await dir.getFileHandle(OPFS_STAGING_NAME, { create: false });
    await deliverOpfsViaServiceWorker(handle, filename, sink.total);
  };
  return sink;
}

// Hand a completed OPFS-staged file off to the user as a download, streaming it
// through `download-sw.js` so peak retained memory is one stream chunk, not the
// whole file. The page reads the OPFS file as a `ReadableStream` and forwards
// each chunk to the worker over a MessagePort, sending only when the worker's
// consumer pulls (backpressure). A hidden iframe navigation to the synthetic URL
// makes the browser treat the streamed Response as an attachment download.
async function deliverOpfsViaServiceWorker(handle, filename, total) {
  const reg = await ensureServiceWorker();
  if (!reg) {
    // No service worker: leave the file staged in OPFS (a test can still read it
    // back) but surface that direct delivery was not possible.
    console.warn('OPFS staged but no service worker for hand-off; file is in OPFS only');
    return;
  }
  const worker = reg.active || navigator.serviceWorker.controller;
  if (!worker) {
    await navigator.serviceWorker.ready;
  }
  const active = reg.active || navigator.serviceWorker.controller;
  if (!active) {
    console.warn('OPFS staged but service worker did not activate; file is in OPFS only');
    return;
  }

  const file = await handle.getFile();
  const id = randomId();
  const channel = new MessageChannel();
  const port = channel.port1;

  // Backpressure: the worker posts `pull` when its consumer drains below the
  // high-water mark; the page sends the next chunk only then.
  let resolvePull = null;
  let pulls = 0;
  let cancelled = false;
  port.onmessage = (ev) => {
    const msg = ev.data;
    if (msg && msg.type === 'pull') {
      if (resolvePull) {
        const r = resolvePull;
        resolvePull = null;
        r();
      } else {
        pulls += 1;
      }
    } else if (msg && (msg.type === 'cancel')) {
      cancelled = true;
      if (resolvePull) {
        const r = resolvePull;
        resolvePull = null;
        r();
      }
    }
  };
  const awaitPull = () => {
    if (cancelled || pulls > 0) {
      if (pulls > 0) pulls -= 1;
      return Promise.resolve();
    }
    return new Promise((resolve) => { resolvePull = resolve; });
  };

  active.postMessage(
    {
      type: 'register',
      id,
      filename: filename || 'swarm-download.bin',
      total: total != null && Number.isFinite(total) ? total : (file.size || null),
      port: channel.port2,
    },
    [channel.port2],
  );

  // Trigger the attachment download via a hidden iframe so the page is not
  // navigated away; the worker answers it with the streamed Response.
  const dlUrl = resolveRootUrl(DL_PREFIX + '/' + id);
  const iframe = document.createElement('iframe');
  iframe.hidden = true;
  iframe.src = dlUrl;
  document.body.appendChild(iframe);

  // Pump the OPFS file's ReadableStream to the worker, one chunk per pull.
  const reader = file.stream().getReader();
  try {
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      if (cancelled) break;
      await awaitPull();
      if (cancelled) break;
      // Copy into a transferable buffer; transfer to keep memory bounded.
      const buf = value.buffer.slice(value.byteOffset, value.byteOffset + value.byteLength);
      port.postMessage(buf, [buf]);
    }
    if (!cancelled) {
      port.postMessage({ type: 'end' });
    }
  } catch (err) {
    try { port.postMessage({ type: 'abort' }); } catch (_) {}
    console.warn('OPFS service-worker hand-off failed', err);
  } finally {
    try { reader.releaseLock(); } catch (_) {}
    setTimeout(() => {
      try { iframe.remove(); } catch (_) {}
    }, 2000);
  }
}

export async function createRandomAccessSink(filename, sizeHint) {
  // FAST PATH: File System Access. The picker REQUIRES an active user gesture,
  // consumed by any prior await, so the caller opens it synchronously in the
  // click handler (see `openRandomAccessPicker`) and stashes the handle on
  // `window.__swarmRaSaveHandle`. If present, stream to it; on any picker
  // failure other than user cancel, fall through to OPFS.
  const pending = typeof window !== 'undefined' ? window.__swarmRaSaveHandle : null;
  if (pending) {
    window.__swarmRaSaveHandle = null;
    const sink = await fsaSinkFrom(pending, sizeHint);
    if (sink) {
      return sink;
    }
  } else if (typeof window !== 'undefined' && typeof window.showSaveFilePicker === 'function') {
    // No in-gesture handle prepared (non-UI caller): try the picker directly.
    const handle = window.showSaveFilePicker({
      suggestedName: filename || 'swarm-download.bin',
    });
    const sink = await fsaSinkFrom(handle, sizeHint);
    if (sink) {
      return sink;
    }
  }

  // FALLBACK: OPFS. Available in every modern engine, needs no gesture, and is
  // the path a headless test drives and reads back to byte-verify.
  if (typeof navigator !== 'undefined' && navigator.storage && navigator.storage.getDirectory) {
    return opfsSink(filename, sizeHint);
  }

  throw new Error('no random-access download path: missing File System Access and OPFS');
}

// Open the FSA save picker synchronously, in the click gesture, stashing the
// handle for the async factory. Returns true when a picker was opened (FSA
// available); false when it is not and OPFS will stage the file. Must be called
// before any await.
export function openRandomAccessPicker(filename) {
  if (typeof window === 'undefined' || typeof window.showSaveFilePicker !== 'function') {
    return false;
  }
  window.__swarmRaSaveHandle = window.showSaveFilePicker({
    suggestedName: filename || 'swarm-download.bin',
  });
  // Swallow the unhandled rejection here; the consumer awaits and handles it.
  window.__swarmRaSaveHandle.catch(() => {});
  return true;
}

// Read the OPFS-staged file back as an ArrayBuffer, for byte-verification after
// a headless random-access download. Returns null if no staged file exists.
export async function readBackOpfsStaged() {
  if (typeof navigator === 'undefined' || !navigator.storage || !navigator.storage.getDirectory) {
    return null;
  }
  const dir = await navigator.storage.getDirectory();
  const handle = await dir.getFileHandle(OPFS_STAGING_NAME, { create: false });
  const file = await handle.getFile();
  return file.arrayBuffer();
}

// Expose for the wasm glue and for tests/manual use.
if (typeof window !== 'undefined') {
  window.createRandomAccessSink = createRandomAccessSink;
  window.openRandomAccessPicker = openRandomAccessPicker;
  window.readBackOpfsStaged = readBackOpfsStaged;
}
