// Stream-to-disk service worker for the Firefox/Safari download fallback.
//
// Browsers without the File System Access API cannot stream a generated file to
// disk directly. This worker mints a synthetic same-origin URL per download and
// answers a navigation to it with a streamed `Response` whose body is fed from
// `postMessage` chunks sent by the page. The page applies backpressure through
// the stream's own pull signal, so memory stays bounded.
//
// Protocol:
//   page -> sw  : { type: 'register', id, filename, total|null, port }
//   page -> port: ArrayBuffer chunk  (a file segment, in order)
//   page -> port: { type: 'end' }    (no more chunks)
//   page -> port: { type: 'abort' }  (cancel; the stream errors)
//   navigation to /__stream_dl__/<id> resolves to the attachment Response.

const PREFIX = '__stream_dl__';

// id -> { filename, total, stream controller wiring }
const downloads = new Map();

self.addEventListener('install', (event) => {
  // Take over without waiting for old clients to release.
  self.skipWaiting();
});

self.addEventListener('activate', (event) => {
  event.waitUntil(self.clients.claim());
});

self.addEventListener('message', (event) => {
  const data = event.data;
  if (!data || data.type !== 'register') {
    return;
  }
  const { id, filename, total, port } = data;
  downloads.set(id, { filename, total, port });

  // Drive a ReadableStream from the page's port. The page sends ArrayBuffer
  // chunks and a final `{ type: 'end' }`; backpressure is signalled back over
  // the same port so the page only sends when the consumer pulls.
  port.onmessage = (ev) => {
    const msg = ev.data;
    const entry = downloads.get(id);
    if (!entry || !entry.controller) {
      // Response not yet requested: buffer minimally by stashing on the entry.
      if (entry) {
        entry.queued = entry.queued || [];
        entry.queued.push(msg);
      }
      return;
    }
    pump(entry, msg);
  };
});

function pump(entry, msg) {
  if (msg && msg.type === 'end') {
    try {
      entry.controller.close();
    } catch (_) {}
    return;
  }
  if (msg && msg.type === 'abort') {
    try {
      entry.controller.error(new Error('download aborted by page'));
    } catch (_) {}
    return;
  }
  // Otherwise an ArrayBuffer (or view) chunk.
  entry.controller.enqueue(new Uint8Array(msg));
  // Ask the page for more once the consumer has drained below the high-water
  // mark. desiredSize <= 0 means full; the page waits for the next 'pull'.
  if (entry.controller.desiredSize !== null && entry.controller.desiredSize > 0) {
    entry.port.postMessage({ type: 'pull' });
  }
}

self.addEventListener('fetch', (event) => {
  const url = new URL(event.request.url);
  const parts = url.pathname.split('/').filter(Boolean);
  const idx = parts.indexOf(PREFIX);
  if (idx === -1 || idx + 1 >= parts.length) {
    return; // not ours; default handling
  }
  const id = parts[idx + 1];
  const entry = downloads.get(id);
  if (!entry) {
    event.respondWith(new Response('unknown download', { status: 404 }));
    return;
  }

  const stream = new ReadableStream({
    start(controller) {
      entry.controller = controller;
      // Flush anything that arrived before the navigation hit us.
      if (entry.queued) {
        for (const msg of entry.queued) {
          pump(entry, msg);
        }
        entry.queued = null;
      }
      // Prime the page for the first chunk.
      entry.port.postMessage({ type: 'pull' });
    },
    pull() {
      entry.port.postMessage({ type: 'pull' });
    },
    cancel() {
      entry.port.postMessage({ type: 'cancel' });
      downloads.delete(id);
    },
  });

  const headers = {
    'content-type': 'application/octet-stream',
    'content-disposition':
      'attachment; filename="' + encodeURIComponent(entry.filename) + '"',
  };
  if (entry.total != null) {
    headers['content-length'] = String(entry.total);
  }
  event.respondWith(new Response(stream, { headers }));
});
