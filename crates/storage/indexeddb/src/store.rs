//! In-memory write-through store implementing the synchronous [`Database`] trait.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use parking_lot::RwLock;
use vertex_storage::{
    Database, DatabaseError, DatabaseErrorInfo, DbTx, DbTxMut, Decode, Encode, Table,
};

use crate::persist::Persister;

/// One table: encoded key bytes to serialized (postcard) value bytes, kept
/// ordered so cursor and `entries` iteration follow key order like redb.
type TableMap = BTreeMap<Vec<u8>, Vec<u8>>;

/// The authoritative in-memory state: a map of table name to its entries.
type State = HashMap<String, TableMap>;

/// IndexedDB-backed database: an in-memory authoritative map, mirrored to
/// IndexedDB on commit by a fire-and-forget task.
///
/// The map answers reads and writes synchronously; the IndexedDB write is
/// best-effort and never blocks a caller. Hydrate the map from a prior session
/// with [`IndexedDbDatabase::open`] before serving reads.
pub struct IndexedDbDatabase {
    state: Arc<RwLock<State>>,
    persister: Persister,
}

impl IndexedDbDatabase {
    /// Open the named database, creating object stores for `tables` and
    /// hydrating the in-memory map from any persisted entries.
    ///
    /// Browser-only and async because the initial IndexedDB read is async; once
    /// open, the [`Database`] surface is fully synchronous.
    pub async fn open(name: &str, tables: &[&str]) -> Result<Self, DatabaseError> {
        let (persister, hydrated) = Persister::open(name, tables).await?;
        let mut state: State = HashMap::with_capacity(tables.len());
        for table in tables {
            state.insert((*table).to_string(), TableMap::new());
        }
        for (table, key, value) in hydrated {
            state.entry(table).or_default().insert(key, value);
        }
        Ok(Self {
            state: Arc::new(RwLock::new(state)),
            persister,
        })
    }

    /// A database backed only by the in-memory mirror, with no IndexedDB sink.
    /// Exercises the synchronous [`Database`] surface without a browser.
    #[cfg(test)]
    pub(crate) fn in_memory(tables: &[&str]) -> Self {
        let mut state: State = HashMap::with_capacity(tables.len());
        for table in tables {
            state.insert((*table).to_string(), TableMap::new());
        }
        Self {
            state: Arc::new(RwLock::new(state)),
            persister: Persister::noop(),
        }
    }

    /// Wrap in an `Arc` for shared ownership.
    #[must_use]
    pub fn into_arc(self) -> Arc<Self> {
        Arc::new(self)
    }
}

impl Database for IndexedDbDatabase {
    type TX = IndexedDbTx;
    type TXMut = IndexedDbTxMut;

    fn tx(&self) -> Result<Self::TX, DatabaseError> {
        Ok(IndexedDbTx {
            state: Arc::clone(&self.state),
        })
    }

    fn tx_mut(&self) -> Result<Self::TXMut, DatabaseError> {
        Ok(IndexedDbTxMut {
            state: Arc::clone(&self.state),
            persister: self.persister.clone(),
            ops: parking_lot::Mutex::new(Vec::new()),
        })
    }
}

fn deserialize<T: Table>(raw: &[u8]) -> Result<T::Value, DatabaseError> {
    postcard::from_bytes(raw).map_err(|e| {
        DatabaseError::Read(DatabaseErrorInfo::with_source(
            format!("deserialize {}", T::NAME),
            e,
        ))
    })
}

fn read_get<T: Table>(state: &State, key: &[u8]) -> Result<Option<T::Value>, DatabaseError> {
    match state.get(T::NAME).and_then(|t| t.get(key)) {
        Some(raw) => Ok(Some(deserialize::<T>(raw)?)),
        None => Ok(None),
    }
}

fn read_entries<T: Table>(state: &State) -> Result<Vec<(T::Key, T::Value)>, DatabaseError> {
    let Some(table) = state.get(T::NAME) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(table.len());
    for (k, v) in table {
        out.push((T::Key::decode(k)?, deserialize::<T>(v)?));
    }
    Ok(out)
}

/// Read-only transaction: a snapshot-free shared read over the in-memory map.
pub struct IndexedDbTx {
    state: Arc<RwLock<State>>,
}

impl DbTx for IndexedDbTx {
    fn get<T: Table>(&self, key: T::Key) -> Result<Option<T::Value>, DatabaseError> {
        read_get::<T>(&self.state.read(), key.encode().as_ref())
    }

    fn entries<T: Table>(&self) -> Result<Vec<(T::Key, T::Value)>, DatabaseError> {
        read_entries::<T>(&self.state.read())
    }

    fn count<T: Table>(&self) -> Result<usize, DatabaseError> {
        Ok(self.state.read().get(T::NAME).map_or(0, BTreeMap::len))
    }
}

/// A staged mutation, applied to the map and mirrored to IndexedDB on commit.
pub(crate) enum Op {
    Put {
        table: &'static str,
        key: Vec<u8>,
        value: Vec<u8>,
    },
    Delete {
        table: &'static str,
        key: Vec<u8>,
    },
    Clear {
        table: &'static str,
    },
}

impl Op {
    /// The table this op targets.
    pub(crate) fn table(&self) -> &'static str {
        match self {
            Op::Put { table, .. } | Op::Delete { table, .. } | Op::Clear { table } => table,
        }
    }
}

/// Read-write transaction: writes stage in `ops`, become visible to the map and
/// are scheduled for persistence on [`commit`](DbTxMut::commit). Reads inside the
/// transaction see staged writes layered over the committed map.
pub struct IndexedDbTxMut {
    state: Arc<RwLock<State>>,
    persister: Persister,
    ops: parking_lot::Mutex<Vec<Op>>,
}

impl IndexedDbTxMut {
    /// The staged value for `key` in `table`: `Some(None)` means staged-deleted,
    /// `Some(Some(bytes))` staged-put, `None` means no staged op touches it.
    fn staged(&self, table: &str, key: &[u8]) -> Option<Option<Vec<u8>>> {
        let ops = self.ops.lock();
        let mut latest = None;
        for op in ops.iter() {
            match op {
                Op::Put {
                    table: t,
                    key: k,
                    value,
                } if *t == table && k == key => latest = Some(Some(value.clone())),
                Op::Delete { table: t, key: k } if *t == table && k == key => latest = Some(None),
                Op::Clear { table: t } if *t == table => latest = Some(None),
                _ => {}
            }
        }
        latest
    }
}

impl DbTx for IndexedDbTxMut {
    fn get<T: Table>(&self, key: T::Key) -> Result<Option<T::Value>, DatabaseError> {
        let encoded = key.encode();
        match self.staged(T::NAME, encoded.as_ref()) {
            Some(Some(raw)) => Ok(Some(deserialize::<T>(&raw)?)),
            Some(None) => Ok(None),
            None => read_get::<T>(&self.state.read(), encoded.as_ref()),
        }
    }

    fn entries<T: Table>(&self) -> Result<Vec<(T::Key, T::Value)>, DatabaseError> {
        // Staged writes are not reflected here; current consumers read entries
        // only outside a write transaction. Keep this a committed-map read.
        read_entries::<T>(&self.state.read())
    }

    fn count<T: Table>(&self) -> Result<usize, DatabaseError> {
        Ok(self.state.read().get(T::NAME).map_or(0, BTreeMap::len))
    }
}

impl DbTxMut for IndexedDbTxMut {
    fn commit(self) -> Result<(), DatabaseError> {
        let ops = self.ops.into_inner();
        if ops.is_empty() {
            return Ok(());
        }
        let mut state = self.state.write();
        for op in &ops {
            match op {
                Op::Put { table, key, value } => {
                    state
                        .entry((*table).to_string())
                        .or_default()
                        .insert(key.clone(), value.clone());
                }
                Op::Delete { table, key } => {
                    if let Some(t) = state.get_mut(*table) {
                        t.remove(key);
                    }
                }
                Op::Clear { table } => {
                    if let Some(t) = state.get_mut(*table) {
                        t.clear();
                    }
                }
            }
        }
        drop(state);
        self.persister.persist(ops);
        Ok(())
    }

    fn put<T: Table>(&self, key: T::Key, value: T::Value) -> Result<(), DatabaseError> {
        let serialized = postcard::to_allocvec(&value).map_err(|e| {
            DatabaseError::write(T::NAME, 0, 0, format!("serialize {}: {e}", T::NAME))
        })?;
        self.ops.lock().push(Op::Put {
            table: T::NAME,
            key: key.encode().as_ref().to_vec(),
            value: serialized,
        });
        Ok(())
    }

    fn delete<T: Table>(&self, key: T::Key) -> Result<bool, DatabaseError> {
        let encoded = key.encode();
        let existed = match self.staged(T::NAME, encoded.as_ref()) {
            Some(staged) => staged.is_some(),
            None => self
                .state
                .read()
                .get(T::NAME)
                .is_some_and(|t| t.contains_key(encoded.as_ref())),
        };
        self.ops.lock().push(Op::Delete {
            table: T::NAME,
            key: encoded.as_ref().to_vec(),
        });
        Ok(existed)
    }

    fn clear<T: Table>(&self) -> Result<(), DatabaseError> {
        self.ops.lock().push(Op::Clear { table: T::NAME });
        Ok(())
    }

    fn ensure_table(&self, name: &str) -> Result<(), DatabaseError> {
        self.state.write().entry(name.to_string()).or_default();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::unwrap_used)]
    use super::*;
    use vertex_storage::table;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[derive(
        Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
    )]
    struct TestKey(u32);

    impl Encode for TestKey {
        type Encoded = [u8; 4];
        fn encode(self) -> Self::Encoded {
            self.0.to_be_bytes()
        }
    }

    impl Decode for TestKey {
        fn decode(value: &[u8]) -> Result<Self, DatabaseError> {
            let bytes: [u8; 4] = value.try_into().map_err(|_| DatabaseError::Decode)?;
            Ok(Self(u32::from_be_bytes(bytes)))
        }
    }

    #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
    struct TestValue(String);

    table!(TestTable, "test", TestKey, TestValue);

    #[wasm_bindgen_test]
    fn put_get_round_trip_via_mirror() {
        let db = IndexedDbDatabase::in_memory(&["test"]);

        db.update(|tx| tx.put::<TestTable>(TestKey(1), TestValue("hello".into())))
            .unwrap();

        let got = db.view(|tx| tx.get::<TestTable>(TestKey(1))).unwrap();
        assert_eq!(got, Some(TestValue("hello".into())));

        // Missing key reads as None.
        let miss = db.view(|tx| tx.get::<TestTable>(TestKey(2))).unwrap();
        assert_eq!(miss, None);
    }

    #[wasm_bindgen_test]
    fn delete_and_clear() {
        let db = IndexedDbDatabase::in_memory(&["test"]);
        db.update(|tx| {
            tx.put::<TestTable>(TestKey(1), TestValue("a".into()))?;
            tx.put::<TestTable>(TestKey(2), TestValue("b".into()))?;
            Ok(())
        })
        .unwrap();
        assert_eq!(db.view(|tx| tx.count::<TestTable>()).unwrap(), 2);

        let existed = db.update(|tx| tx.delete::<TestTable>(TestKey(1))).unwrap();
        assert!(existed);
        assert_eq!(db.view(|tx| tx.get::<TestTable>(TestKey(1))).unwrap(), None);

        db.update(|tx| tx.clear::<TestTable>()).unwrap();
        assert_eq!(db.view(|tx| tx.count::<TestTable>()).unwrap(), 0);
    }

    #[wasm_bindgen_test]
    fn overwrite_and_entries_are_key_ordered() {
        let db = IndexedDbDatabase::in_memory(&["test"]);
        db.update(|tx| {
            tx.put::<TestTable>(TestKey(2), TestValue("b".into()))?;
            tx.put::<TestTable>(TestKey(1), TestValue("a".into()))?;
            tx.put::<TestTable>(TestKey(1), TestValue("a2".into()))?;
            Ok(())
        })
        .unwrap();

        let entries = db.view(|tx| tx.entries::<TestTable>()).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].0, TestKey(1));
        assert_eq!(entries[0].1, TestValue("a2".into()));
        assert_eq!(entries[1].0, TestKey(2));
    }
}
