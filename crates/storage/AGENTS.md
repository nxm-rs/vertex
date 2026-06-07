# AGENTS: crates/storage/

Pluggable key/value storage abstraction (`vertex-storage`) and its redb backend (`vertex-storage-redb`). Higher layers (peer manager persistence, storer chunk store, reserve) consume the `Database` and `DbTx` traits, never redb directly.

Root-level rules in `/AGENTS.md` apply here too. The notes below are the area-specific overlay.

## Crates

- `vertex-storage`: traits (`Database`, `DbTxMut`, `DbTxRo`, `Table`), codecs, and the error hierarchy.
- `vertex-storage-redb`: redb-backed implementation with stats and metrics modules.

## Dos

- New tables go behind the `Table` trait so the backend stays swappable.
- Keep codec choices in the `codecs` module. Postcard is the default; if a table needs something else, document why in the table type.
- Surface backend-specific errors through `DatabaseErrorInfo` so the storage trait stays neutral.
- For new write paths, add a write-buffer or batched-transaction strategy. Synchronous transactions are the slow path.
- Expose stats and metrics through the backend's `metrics` submodule, with `strum::IntoStaticStr` on any reason enums.

## Donts

- Do not depend on `vertex-swarm-*` from `vertex-storage` or `vertex-storage-redb`. The dependency direction is storage to consumers, never the reverse.
- Do not leak `redb::Error` outside the backend crate.
- Do not call `unwrap` or `expect` on transaction results. The lints warn but the policy is no exceptions in storage code paths.
- Do not add long-lived background tasks here. Persistence tasks live in the consumer crates so the storage crates remain library-shaped.

## Tests

- `cargo test -p vertex-storage` for the trait surface (uses in-memory fixtures).
- `cargo test -p vertex-storage-redb` covers the on-disk backend; the `InMemoryBackend` exercise also lives there.
- Stats and metrics expectations are part of the redb crate's tests.
