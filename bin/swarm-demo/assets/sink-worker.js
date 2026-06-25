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
// Two ways writes reach this worker, both writing the one OPFS handle:
//
//   1. Single-sender (the random-access sink): the control channel (the worker's
//      own `self.postMessage` port) carries open/write/close, acked on it.
//
//   2. Unified multi-worker download: the coordinator transfers one `MessagePort`
//      per fetch worker over `{ type: 'addPort', port }`. Each fetch worker posts
//      `{ type: 'write', offset, buffer }` to its port and the sink writes it at
//      the absolute file offset and acks on that same port, so K fetch workers
//      stream their slices straight here with no main-thread relay. The handle is
//      the single owner: all writes funnel through one `handle.write`, sequential
//      by construction. `bytesWritten` aggregates across every port and equals the
//      file size at completion.
//
// Protocol (control channel, main thread -> worker):
//   { type: 'open',  filename }                  -> { type: 'opened' } | error
//   { type: 'write', offset, buffer }            -> { type: 'ack', bytesWritten }
//   { type: 'addPort', port }                    -> (no reply; port now live)
//   { type: 'close' }                            -> { type: 'closed', bytesWritten }
// Protocol (per fetch-worker MessagePort):
//   { type: 'write', offset, buffer }            -> { type: 'ack', bytesWritten }
// `buffer` is a transferred ArrayBuffer (zero-copy hand-off). The `ack` is the
// backpressure signal: the sender awaits it before posting the next write, so the
// postMessage queue never grows unboundedly and the write rate paces the fetch.

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

// Synchronous positional write of `buffer` at `offset`, returning the byte count.
// The single owner of the OPFS handle, so every port's writes serialise here.
function writeAt(offset, buffer) {
  if (!handle) {
    throw new Error('sink worker write before open');
  }
  const view = new Uint8Array(buffer);
  const n = handle.write(view, { at: offset });
  bytesWritten += n;
  return n;
}

// Service `write` messages arriving on a fetch worker's MessagePort. Each write
// is acked on the same port so the fetch worker paces to the disk write rate.
function addPort(port) {
  port.onmessage = (ev) => {
    const msg = ev.data;
    if (!msg || msg.type !== 'write') {
      return;
    }
    try {
      writeAt(msg.offset, msg.buffer);
      port.postMessage({ type: 'ack', bytesWritten });
    } catch (err) {
      port.postMessage({ type: 'error', error: String(err && err.message ? err.message : err) });
    }
  };
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
      case 'addPort': {
        addPort(msg.port);
        break;
      }
      case 'write': {
        const n = writeAt(msg.offset, msg.buffer);
        self.postMessage({ type: 'ack', bytesWritten, n });
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
