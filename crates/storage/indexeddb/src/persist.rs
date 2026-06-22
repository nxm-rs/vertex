//! Best-effort IndexedDB mirror of the in-memory map.
//!
//! IndexedDB handles are `!Send`, so a single owning task holds the `idb`
//! database and drains committed write batches off a `Send` channel. The
//! [`Persister`] held by the database is just the channel sender plus the table
//! list, so the [`IndexedDbDatabase`](crate::IndexedDbDatabase) stays
//! `Send + Sync + 'static` as the [`Database`](vertex_storage::Database) trait
//! requires.

use futures::StreamExt;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use idb::{
    Database as IdbDatabase, DatabaseEvent, Factory, KeyPath, ObjectStoreParams, TransactionMode,
};
use vertex_storage::DatabaseError;
use wasm_bindgen_futures::spawn_local;

use crate::store::Op;

/// Key/value column names inside each object store. Each store row is
/// `{ key: <bytes>, value: <bytes> }`, keyed by the `key` column.
const KEY_COLUMN: &str = "key";
const VALUE_COLUMN: &str = "value";

/// A persisted entry: table name, encoded key bytes, serialized value bytes.
pub(crate) type Hydrated = (String, Vec<u8>, Vec<u8>);

/// The mirror sink: forwards committed write batches to the owning task.
#[derive(Clone)]
pub(crate) struct Persister {
    sink: UnboundedSender<Vec<Op>>,
}

impl Persister {
    /// Open the database, create one object store per table, hydrate existing
    /// rows into memory, and spawn the owning persist task.
    pub(crate) async fn open(
        name: &str,
        tables: &[&str],
    ) -> Result<(Self, Vec<Hydrated>), DatabaseError> {
        let db = open_idb(name, tables).await?;
        let hydrated = read_all(&db, tables).await?;

        let (sink, mut stream) = unbounded::<Vec<Op>>();
        spawn_local(async move {
            // The task owns the `!Send` database for its whole lifetime. It ends
            // when every sender is dropped, i.e. the database handle is gone.
            while let Some(batch) = stream.next().await {
                if let Err(err) = apply_batch(&db, batch).await {
                    tracing::debug!(error = %err, "indexeddb mirror write failed");
                }
            }
        });

        Ok((Self { sink }, hydrated))
    }

    /// A sink with no backing task, for exercising the in-memory mirror without
    /// a browser IndexedDB. Every batch is dropped.
    #[cfg(test)]
    pub(crate) fn noop() -> Self {
        let (sink, _drop) = unbounded::<Vec<Op>>();
        Self { sink }
    }

    /// Schedule a committed batch for persistence. Never blocks; a closed sink
    /// (the task ended) silently drops the batch, which is acceptable for the
    /// lossy cache.
    pub(crate) fn persist(&self, ops: Vec<Op>) {
        let _ = self.sink.unbounded_send(ops);
    }
}

fn err(context: &str, e: idb::Error) -> DatabaseError {
    DatabaseError::Other(format!("indexeddb {context}: {e}"))
}

async fn open_idb(name: &str, tables: &[&str]) -> Result<IdbDatabase, DatabaseError> {
    let factory = Factory::new().map_err(|e| err("factory", e))?;
    let mut request = factory.open(name, Some(1)).map_err(|e| err("open", e))?;

    let stores: Vec<String> = tables.iter().map(|t| (*t).to_string()).collect();
    request.on_upgrade_needed(move |event| {
        let Ok(db) = event.database() else {
            return;
        };
        let existing = db.store_names();
        for store in &stores {
            if existing.iter().any(|n: &String| n == store) {
                continue;
            }
            let mut params = ObjectStoreParams::new();
            params.key_path(Some(KeyPath::new_single(KEY_COLUMN)));
            let _ = db.create_object_store(store, params);
        }
    });

    request.await.map_err(|e| err("await open", e))
}

async fn read_all(db: &IdbDatabase, tables: &[&str]) -> Result<Vec<Hydrated>, DatabaseError> {
    let mut out = Vec::new();
    for table in tables {
        let tx = db
            .transaction(&[*table], TransactionMode::ReadOnly)
            .map_err(|e| err("read tx", e))?;
        let store = tx.object_store(table).map_err(|e| err("object store", e))?;
        let rows = store
            .get_all(None, None)
            .map_err(|e| err("get_all", e))?
            .await
            .map_err(|e| err("await get_all", e))?;
        for row in rows {
            if let Some((k, v)) = decode_row(&row) {
                out.push(((*table).to_string(), k, v));
            }
        }
        let _ = tx.await;
    }
    Ok(out)
}

async fn apply_batch(db: &IdbDatabase, ops: Vec<Op>) -> Result<(), DatabaseError> {
    use std::collections::BTreeSet;

    let stores: BTreeSet<&'static str> = ops.iter().map(Op::table).collect();
    let names: Vec<&str> = stores.iter().copied().collect();
    let tx = db
        .transaction(&names, TransactionMode::ReadWrite)
        .map_err(|e| err("write tx", e))?;

    for op in &ops {
        let store = tx
            .object_store(op.table())
            .map_err(|e| err("object store", e))?;
        match op {
            Op::Put { key, value, .. } => {
                let row = encode_row(key, value);
                store.put(&row, None).map_err(|e| err("put", e))?;
            }
            Op::Delete { key, .. } => {
                let js_key = bytes_to_js(key);
                store.delete(js_key).map_err(|e| err("delete", e))?;
            }
            Op::Clear { .. } => {
                store.clear().map_err(|e| err("clear", e))?;
            }
        }
    }

    tx.commit()
        .map_err(|e| err("commit", e))?
        .await
        .map(|_| ())
        .map_err(|e| err("await commit", e))
}

fn bytes_to_js(bytes: &[u8]) -> wasm_bindgen::JsValue {
    js_sys::Uint8Array::from(bytes).into()
}

fn encode_row(key: &[u8], value: &[u8]) -> wasm_bindgen::JsValue {
    let obj = js_sys::Object::new();
    let _ = js_sys::Reflect::set(
        &obj,
        &KEY_COLUMN.into(),
        &js_sys::Uint8Array::from(key).into(),
    );
    let _ = js_sys::Reflect::set(
        &obj,
        &VALUE_COLUMN.into(),
        &js_sys::Uint8Array::from(value).into(),
    );
    obj.into()
}

fn decode_row(row: &wasm_bindgen::JsValue) -> Option<(Vec<u8>, Vec<u8>)> {
    let key = js_sys::Reflect::get(row, &KEY_COLUMN.into()).ok()?;
    let value = js_sys::Reflect::get(row, &VALUE_COLUMN.into()).ok()?;
    let key = js_sys::Uint8Array::new(&key).to_vec();
    let value = js_sys::Uint8Array::new(&value).to_vec();
    Some((key, value))
}
