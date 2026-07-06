# AGENTS: crates/storage/

Pluggable key/value storage abstraction (`vertex-storage`) and its redb backend (`vertex-storage-redb`). Consumers (peer-manager persistence, storer chunk store, reserve) use the `Database` and `DbTx`/`DbTxMut` traits, never redb directly.

Global rules: see root `/AGENTS.md`. The notes below are the area-specific overlay.

## Crates

- `vertex-storage`: traits (`Database`, `DbTx`, `DbTxMut`, `Table`), codecs, and the error hierarchy.
- `vertex-storage-redb`: redb-backed implementation with `stats` and `metrics` modules.
- `vertex-storage-indexeddb`: browser-only (`cfg(target_arch = "wasm32")`) `Database` impl for the wasm client cache. The trait is synchronous and IndexedDB is async, so it is an in-memory authoritative map mirrored to IndexedDB by a fire-and-forget `spawn_local` task; durability is best-effort. That persist task is the one sanctioned long-lived task in a storage crate, because the IndexedDB handle is `!Send` and cannot live in the consumer; it terminates when the database is dropped.

## Table versioning and migration policy

Tables have no in-band schema version marker. The version lives in the table
name, and evolution is by rename. This section is the policy every table author
follows; the `table!` registry in `crates/storage/src/table.rs` is the seam it
hangs off.

### Table-name versioning

- A table name identifies both the logical table and its on-disk value shape.
  When a value type changes in a way that an old serialized record can no longer
  decode into, that is a new table: pick a new name (a `_v2` suffix is the
  obvious convention) and register it. Never redefine a live name to a
  decode-incompatible type, or startup reads panic on the first old record.
- The registry (`Tables::NAMES`, or the `&[&str]` slice a consumer hands to
  `init_tables`) is authoritative for what a database should contain. The old,
  renamed table is not in the registry; it is left on disk and ignored. That is
  the whole of today's "rename and ignore" migration, and it is why orphaned
  tables accumulate. The startup sweep below is the counterweight.
- A compatible value change (a new optional field an old record decodes into, an
  additive enum variant) keeps the same name and needs no new table. Prefer this:
  postcard plus the `Value` bound tolerates additive `serde` evolution, so most
  changes never need a rename.

### Replay-idempotent writes

Every write path must be safe to replay: re-running the same initialization or
the same store after a crash-and-restart must converge to the same state, never
corrupt or double-count. The trait surface is built for this and callers must
keep it:

- `ensure_table` is create-if-absent, so re-initializing an existing database is
  a no-op. `put` is last-write-wins (an overwrite, not an append); `delete`
  reports whether the key existed but is otherwise idempotent.
- A store that owns a whole table replaces it inside one transaction
  (`clear` then re-`put`, as the peer-snapshot store does) so a partial write is
  never committed and a replay simply rewrites the same rows.
- Keep secondary-index maintenance on the `IndexedWrite` helpers; they delete the
  stale index entry before writing the new one, which is what makes a replayed
  `put_indexed` idempotent rather than orphaning an index row.

### Degrade-to-memory on open failure

Opening the on-disk database is best-effort, not fatal. When a configured path
fails to open, the node degrades to in-memory operation rather than aborting the
build: it stays available and serves traffic, at the cost of persistence
(peer snapshots and any other persisted state are lost on restart, and the
operator is warned). Availability is chosen over durability because the store is
a cache-shaped convenience, not a consensus record. The launch path
(`crates/swarm/builder/src/launch.rs`, `open_shared_database`) owns and tests
this decision; `open_database` surfaces the open error and `RedbDatabase::in_memory`
is the fallback it degrades onto. Do not turn an open failure into a hard node
abort.

### Startup sweep for unknown tables

`sweep_unknown_tables(db, registry, policy)` (in `crates/storage/src/sweep.rs`)
compares the tables physically present against the registry and reports any that
are absent from it. It reads the registry passed to it at call time and never
hardcodes table names, so a table added by any consumer (accounting, chequebook,
a future storer table) is covered the moment that consumer includes it in the
slice it initializes. Backends that cannot enumerate their tables return
`Ok(None)` from `Database::table_names` and the sweep is a no-op for them.

The sweep defaults to `UnknownTablePolicy::Log`: an unknown table is logged and
retained. Dropping is deliberately opt-in because a table missing from the
current registry is not proof of an orphan. It may be a not-yet-deployed feature
whose registry entry lands in a later binary, or an older-version table that a
migration still needs to read. Vacuuming it would silently destroy live or
soon-to-be-live data. Logging leaks disk but never data, so it is the safe
default and the sweep never touches a registered table under any policy.

`UnknownTablePolicy::Vacuum` drops the unknown tables and is safe only once the
table is a confirmed retired schema: its name will never re-enter any deployed
registry, and no in-flight migration still reads it. In practice that means an
operator-initiated action after a version is fully rolled out, not an automatic
step on every boot.

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
