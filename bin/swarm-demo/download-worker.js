// OPFS download worker.
//
// Owns a sandboxed Origin Private File System scratch file through a synchronous
// access handle, which the spec exposes only in a dedicated worker. That lets
// the random-access, out-of-order positional writes the joiner emits run off the
// page's fetch thread: the synchronous OPFS write blocks this worker, never the
// thread driving libp2p retrieval. The main thread posts one write at a time and
// awaits an ack, so peak memory is one chunk and the file lives on disk, not the
// heap.
//
// Protocol, main -> worker:
//   { type: 'open',     name }
//   { type: 'truncate', seq, len }
//   { type: 'write',    seq, offset, buffer }   // buffer transferred
//   { type: 'close' }
//   { type: 'abort' }
// worker -> main:
//   { type: 'opened' } | { type: 'ack', seq } | { type: 'closed' }
//   | { type: 'aborted' } | { type: 'error', seq, message }

let access = null;

self.onmessage = async (ev) => {
  const msg = ev.data;
  if (!msg) return;
  try {
    switch (msg.type) {
      case 'open': {
        const root = await navigator.storage.getDirectory();
        const handle = await root.getFileHandle(msg.name, { create: true });
        // createSyncAccessHandle is dedicated-worker only; this is why the sink
        // needs a worker rather than a main-thread OPFS writable.
        access = await handle.createSyncAccessHandle();
        self.postMessage({ type: 'opened' });
        break;
      }
      case 'truncate': {
        if (!access) throw new Error('truncate before open');
        // Pre-size so every in-range positional write lands in a sparse file.
        access.truncate(msg.len);
        self.postMessage({ type: 'ack', seq: msg.seq });
        break;
      }
      case 'write': {
        if (!access) throw new Error('write before open');
        const view = new Uint8Array(msg.buffer);
        const wrote = access.write(view, { at: msg.offset });
        // A short write (e.g. quota exhaustion) must fail loudly rather than
        // leave a silently truncated file.
        if (wrote !== view.byteLength) {
          throw new Error('short write at ' + msg.offset + ': ' + wrote + '/' + view.byteLength);
        }
        self.postMessage({ type: 'ack', seq: msg.seq });
        break;
      }
      case 'close': {
        if (access) {
          access.flush();
          access.close();
          access = null;
        }
        self.postMessage({ type: 'closed' });
        break;
      }
      case 'abort': {
        if (access) {
          try {
            access.close();
          } catch (_) {}
          access = null;
        }
        self.postMessage({ type: 'aborted' });
        break;
      }
    }
  } catch (e) {
    self.postMessage({ type: 'error', seq: msg.seq, message: String((e && e.message) || e) });
  }
};
