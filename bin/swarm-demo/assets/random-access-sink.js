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
// so a slow chunk never blocks the writes ready behind it. Both back-ends
// support positional writes on an async `FileSystemWritableFileStream` via the
// `{ type: 'write', position, data }` write command, which is main-thread safe
// (no `SyncAccessHandle` worker needed).
//
// Two implementations, feature-detected at call time:
//
//   * File System Access (Chromium/Edge): `showSaveFilePicker` opens the native
//     save dialog and the writable streams straight to the picked file. The
//     picker REQUIRES a user gesture, so this factory consumes a handle opened
//     in-gesture by `openRandomAccessPicker` (stashed on `window`).
//
//   * OPFS (Firefox/Chromium/headless): the origin-private file system stages
//     the file under a fixed name. `navigator.storage.getDirectory` needs no
//     gesture, so this path is driveable headlessly: the test opens the same
//     OPFS file, reads it back, and hashes it to byte-verify the assembly.
//
// Keeping the browser plumbing here lets the Rust side stay a plain `extern`
// binding with async writeAt/close/abort/setTotal.

// Fixed staging name for the OPFS back-end, so a test can re-open and read back
// the assembled file to byte-verify it.
const OPFS_STAGING_NAME = '__swarm_ra_download__';

// Progress is surfaced through the same window-level hook the ordered sink
// uses; absent that it is a no-op, so the sink stays usable from a bare page.
function notifyProgress(written, total) {
  if (typeof window !== 'undefined' && typeof window.__swarmDownloadProgress === 'function') {
    window.__swarmDownloadProgress(written, total);
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

// Build a sink over an OPFS-staged file. No gesture required; the file lands
// under `OPFS_STAGING_NAME` so a test can re-open and read it back.
async function opfsSink(sizeHint) {
  const dir = await navigator.storage.getDirectory();
  const handle = await dir.getFileHandle(OPFS_STAGING_NAME, { create: true });
  const writable = await handle.createWritable({ keepExistingData: true });
  const sink = new WritablePositionSink(writable);
  if (sizeHint != null && Number.isFinite(sizeHint)) {
    sink.setTotal(sizeHint);
  }
  return sink;
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
    return opfsSink(sizeHint);
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
