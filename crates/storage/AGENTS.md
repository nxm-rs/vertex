# AGENTS: crates/storage/

Pluggable key/value storage abstraction (`vertex-storage`) and its redb backend (`vertex-storage-redb`). Consumers (peer-manager persistence, storer chunk store, reserve) use the `Database` and `DbTx`/`DbTxMut` traits, never redb directly.

Global rules: see root `/AGENTS.md`. The notes below are the area-specific overlay.

## Crates

- `vertex-storage`: traits (`Database`, `DbTx`, `DbTxMut`, `Table`), codecs, and the error hierarchy.
- `vertex-storage-redb`: redb-backed implementation with `stats` and `metrics` modules.
- `vertex-storage-indexeddb`: browser-only (`cfg(target_arch = "wasm32")`) `Database` impl for the wasm client cache. The trait is synchronous and IndexedDB is async, so it is an in-memory authoritative map mirrored to IndexedDB by a fire-and-forget `spawn_local` task; durability is best-effort. That persist task is the one sanctioned long-lived task in a storage crate, because the IndexedDB handle is `!Send` and cannot live in the consumer; it terminates when the database is dropped.

## Dos

- New tables go behind the `Table` trait so the backend stays swappable.
- Keep codec choices in the `codecs` module. Postcard is the default; if a table needs something else, document why in the table type.
- Surface backend-specific errors through `DatabaseErrorInfo` so the storage trait stays neutral.
- For new write paths, add a write-buffer or batched-transaction strategy. Synchronous transactions are the slow path.
- Expose stats and metrics through the backend's `metrics` submodule, with `strum::IntoStaticStr` on any reason enums.

## Donts

- Do not depend on `vertex-swarm-*` from `vertex-storage` or `vertex-storage-redb`. Dependency direction is storage to consumers, never the reverse.
- Do not leak `redb::Error` outside the backend crate.
- Do not call `unwrap` or `expect` on transaction results. No exceptions in storage code paths.
- Do not add long-lived background tasks here. Persistence tasks live in the consumer crates so the storage crates stay library-shaped.

## Tests

- `cargo test -p vertex-storage` for the trait surface (in-memory fixtures).
- `cargo test -p vertex-storage-redb` covers the on-disk backend; the `InMemoryBackend` exercise and the stats/metrics expectations live there too.
