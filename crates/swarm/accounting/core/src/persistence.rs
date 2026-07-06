//! Write-behind persistence for per-peer balances.
//!
//! Balances are in-memory atomics on the hot path; without a store a restart
//! forgets what we owe, so a debtor-initiated settlement never repays a creditor
//! whose ledger keeps growing until it blocklists us. The store carries the
//! signed balance across restarts: reloaded before the accounting serves any
//! request, flushed from a dirty set on a periodic tick and on shutdown. A crash
//! loses at most one flush interval, never the whole ledger.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use vertex_storage::{Database, DatabaseError, DbTx, DbTxMut, Table, table};
use vertex_swarm_primitives::OverlayAddress;

// One balance table keyed by overlay. A single signed value carries the whole
// per-peer ledger (positive: the peer owes us; negative: we owe the peer), so
// one field is every side the restore needs.
table!(pub(crate) BalanceTable, "accounting_balances", OverlayAddress, PersistedBalance);

/// A peer's durable signed balance in AU.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct PersistedBalance {
    /// Signed balance: positive means the peer owes us, negative we owe the peer.
    pub balance: i64,
}

/// Persistence failure for the balance store.
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
pub enum BalanceStoreError {
    /// The underlying database rejected a load or flush.
    #[error("balance store: {0}")]
    Storage(String),
}

fn db_err(e: DatabaseError) -> BalanceStoreError {
    BalanceStoreError::Storage(e.to_string())
}

/// A backing store for per-peer balances.
///
/// Object-safe so the hot-path [`Accounting`](crate::Accounting) holds
/// `Option<Arc<dyn BalanceStore>>` and never gains a database type parameter.
pub trait BalanceStore: Send + Sync {
    /// Load every persisted balance (called once at construction).
    fn load(&self) -> Result<Vec<(OverlayAddress, PersistedBalance)>, BalanceStoreError>;

    /// Upsert the given balances. Incremental: only the dirty peers are written,
    /// and entries for untouched peers are left in place.
    fn flush(
        &self,
        records: &[(OverlayAddress, PersistedBalance)],
    ) -> Result<(), BalanceStoreError>;
}

/// Balance store over the vertex-storage [`Database`] trait.
///
/// Generic over the backend so a non-redb store (for example a wasm-targeted
/// one) slots in unchanged.
pub struct DbBalanceStore<DB: Database> {
    db: Arc<DB>,
}

impl<DB: Database> DbBalanceStore<DB> {
    /// Wrap a shared database.
    pub fn new(db: Arc<DB>) -> Self {
        Self { db }
    }

    /// Ensure the balance table exists (call once at startup).
    pub fn init(&self) -> Result<(), BalanceStoreError> {
        self.db
            .update(|tx| tx.ensure_table(BalanceTable::NAME))
            .map_err(db_err)
    }
}

impl<DB: Database> BalanceStore for DbBalanceStore<DB> {
    fn load(&self) -> Result<Vec<(OverlayAddress, PersistedBalance)>, BalanceStoreError> {
        self.db
            .view(|tx| tx.entries::<BalanceTable>())
            .map_err(db_err)
    }

    fn flush(
        &self,
        records: &[(OverlayAddress, PersistedBalance)],
    ) -> Result<(), BalanceStoreError> {
        self.db
            .update(|tx| {
                for (peer, record) in records {
                    tx.put::<BalanceTable>(*peer, *record)?;
                }
                Ok(())
            })
            .map_err(db_err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> DbBalanceStore<vertex_storage_redb::RedbDatabase> {
        let db = vertex_storage_redb::RedbDatabase::in_memory()
            .unwrap()
            .into_arc();
        let store = DbBalanceStore::new(db);
        store.init().unwrap();
        store
    }

    fn peer(n: u8) -> OverlayAddress {
        OverlayAddress::from([n; 32])
    }

    #[test]
    fn roundtrip_preserves_balances() {
        let store = setup();
        store
            .flush(&[
                (peer(1), PersistedBalance { balance: -500 }),
                (peer(2), PersistedBalance { balance: 1200 }),
            ])
            .unwrap();

        let mut loaded = store.load().unwrap();
        loaded.sort_by_key(|(p, _)| *p);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].1.balance, -500);
        assert_eq!(loaded[1].1.balance, 1200);
    }

    #[test]
    fn flush_is_incremental_upsert() {
        let store = setup();
        store
            .flush(&[(peer(1), PersistedBalance { balance: -500 })])
            .unwrap();
        // A later flush of a different peer must not drop the first.
        store
            .flush(&[(peer(2), PersistedBalance { balance: 7 })])
            .unwrap();
        // Re-flushing peer 1 overwrites in place.
        store
            .flush(&[(peer(1), PersistedBalance { balance: -900 })])
            .unwrap();

        let loaded = store.load().unwrap();
        assert_eq!(loaded.len(), 2);
        let one = loaded.iter().find(|(p, _)| *p == peer(1)).unwrap();
        assert_eq!(one.1.balance, -900);
    }

    #[test]
    fn load_on_fresh_store_is_empty() {
        let store = setup();
        assert!(store.load().unwrap().is_empty());
    }
}
