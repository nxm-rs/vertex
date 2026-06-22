// Browser download sink factory.
//
// `createDownloadSink(filename, sizeHintOrNull)` returns a sink the Rust wasm
// streaming download writes ordered segments to:
//
//   { write(Uint8Array): Promise, setTotal(n), close(): Promise, abort(reason) }
//
// Two implementations, feature-detected at call time:
//
//   * File System Access (Chromium/Edge): `showSaveFilePicker` opens the native
//     save dialog and streams straight to a `FileSystemWritableFileStream`. The
//     picker REQUIRES a user gesture, so this factory must be called inside the
//     download click handler, before any awaiting.
//
//   * Service worker (Firefox/Safari): a StreamSaver-style worker answers a
//     navigation to a synthetic URL with a streamed attachment Response fed from
//     a MessagePort. Backpressure flows back over the port as `pull` signals.
//
// Keeping all browser-specific plumbing here lets the Rust side stay a plain
// `extern` binding with async write/close/abort/setTotal and no unstable cfgs.

const SW_URL = resolveRootUrl('download-sw.js');
const DL_PREFIX = '__stream_dl__';

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

// Progress is surfaced through a window-level hook the UI installs; absent that
// it is a no-op, so the sink stays usable from a bare page.
function notifyProgress(written, total) {
  if (typeof window !== 'undefined' && typeof window.__swarmDownloadProgress === 'function') {
    window.__swarmDownloadProgress(written, total);
  }
}

// Build an FsaSink from a (possibly pending) save handle. Re-throws an
// AbortError verbatim (user cancel, surfaced cleanly), and returns null on any
// other failure so the caller falls back to the service-worker sink.
async function fsaSinkFrom(handlePromise, sizeHint) {
  try {
    const handle = await handlePromise;
    const writable = await handle.createWritable();
    const sink = new FsaSink(writable);
    if (sizeHint != null) {
      sink.setTotal(sizeHint);
    }
    return sink;
  } catch (err) {
    if (err && err.name === 'AbortError') {
      const cancelled = new Error('save cancelled');
      cancelled.name = 'AbortError';
      throw cancelled;
    }
    // Lost-gesture SecurityError, picker unavailable, write error: fall back.
    console.warn('save picker failed, falling back to service worker', err);
    return null;
  }
}

export async function createDownloadSink(filename, sizeHint) {
  // FAST PATH: File System Access. The picker REQUIRES an active user gesture,
  // which a prior await would have consumed, so the caller opens it
  // synchronously in the click handler (see `openSavePicker`) and hands the
  // resulting handle in via `window.__swarmSaveHandle`. If that handle is
  // present, stream straight to it; if the picker threw for any reason other
  // than a user cancel, fall through to the service-worker sink below.
  const pending = typeof window !== 'undefined' ? window.__swarmSaveHandle : null;
  if (pending) {
    window.__swarmSaveHandle = null;
    const sink = await fsaSinkFrom(pending, sizeHint);
    if (sink) {
      return sink;
    }
  } else if (typeof window !== 'undefined' && typeof window.showSaveFilePicker === 'function') {
    // No in-gesture handle was prepared (non-UI caller, e.g. a test): try the
    // picker directly, with the same cancel/fallback handling.
    const handle = window.showSaveFilePicker({
      suggestedName: filename || 'swarm-download.bin',
    });
    const sink = await fsaSinkFrom(handle, sizeHint);
    if (sink) {
      return sink;
    }
  }

  // FALLBACK: service worker stream. Register (idempotent), open a port, tell
  // the worker about this download, then navigate a hidden iframe to the
  // synthetic URL so the browser treats the streamed Response as a download.
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

// Open the File System Access save picker synchronously, while the click
// gesture is still active, and stash the resulting promise for the async sink
// factory to consume. Returns true when a picker was opened (FSA available), so
// the caller knows the in-gesture path was taken. The promise may reject (user
// cancel, lost gesture); `createDownloadSink` handles that.
export function openSavePicker(filename) {
  if (typeof window === 'undefined' || typeof window.showSaveFilePicker !== 'function') {
    return false;
  }
  // Must be called directly in the gesture: do not await before this line.
  window.__swarmSaveHandle = window.showSaveFilePicker({
    suggestedName: filename || 'swarm-download.bin',
  });
  // Swallow unhandled rejection here; the consumer awaits and handles it.
  window.__swarmSaveHandle.catch(() => {});
  return true;
}

// Expose for the wasm glue and for tests/manual use.
if (typeof window !== 'undefined') {
  window.createDownloadSink = createDownloadSink;
  window.openSavePicker = openSavePicker;
}
