// OPFS sink worker: owns a synchronous access handle to the staged download file
// and services positional writes off the main thread.
//
// `createSyncAccessHandle()` is worker-only and grants an exclusive lock on the
// OPFS file for the worker's lifetime, so the synchronous positional `write`
// (the fast OPFS write that has no main-thread equivalent) runs here while the
// main thread stays free to fetch and decode. The main thread must not open the
// same file until this worker posts `closed`; it then reads the bytes back for
// delivery and verification.
//
// Protocol (main thread -> worker):
//   { type: 'open',  filename }                  -> { type: 'opened' } | error
//   { type: 'write', offset, buffer }            -> { type: 'ack', bytesWritten }
//   { type: 'close' }                            -> { type: 'closed', bytesWritten }
// `buffer` is a transferred ArrayBuffer (zero-copy hand-off). The `ack` is the
// backpressure signal: the sender awaits it before posting the next write, so
// the postMessage queue never grows unboundedly and the write rate paces the
// fetch.

let handle = null;
let bytesWritten = 0;

async function open(filename) {
  const dir = await navigator.storage.getDirectory();
  // The fixed staging name the main thread reads back after close; `filename` is
  // the user-facing name applied only at delivery, not the OPFS entry name.
  const fh = await dir.getFileHandle(filename, { create: true });
  handle = await fh.createSyncAccessHandle();
  handle.truncate(0);
  bytesWritten = 0;
}

self.onmessage = async (ev) => {
  const msg = ev.data;
  try {
    switch (msg && msg.type) {
      case 'open': {
        await open(msg.filename);
        self.postMessage({ type: 'opened' });
        break;
      }
      case 'write': {
        if (!handle) {
          throw new Error('sink worker write before open');
        }
        const view = new Uint8Array(msg.buffer);
        // Synchronous positional write; `at` seeks without truncating. Returns
        // the byte count actually written.
        const n = handle.write(view, { at: msg.offset });
        bytesWritten += n;
        self.postMessage({ type: 'ack', bytesWritten });
        break;
      }
      case 'close': {
        if (handle) {
          handle.flush();
          handle.close();
          handle = null;
        }
        self.postMessage({ type: 'closed', bytesWritten });
        break;
      }
      default:
        throw new Error(`sink worker: unknown message ${msg && msg.type}`);
    }
  } catch (err) {
    self.postMessage({ type: 'error', error: String(err && err.message ? err.message : err) });
  }
};
