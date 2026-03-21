//! Database-backed accounting store for peer state persistence.

use std::fmt;
use std::sync::Arc;

use vertex_storage::{Database, DatabaseError, DbTx, DbTxMut, Table, table};
use vertex_swarm_primitives::OverlayAddress;

use crate::accounting::PeerAccounting;

// Primary table: OverlayAddress → PeerAccounting
table!(pub(crate) AccountingTable, "accounting", OverlayAddress, PeerAccounting);

/// Errors from accounting store operations.
#[derive(Debug, thiserror::Error)]
pub enum AccountingStoreError {
    /// Database operation failed.
    #[error("storage error: {_0}")]
    Storage(String),
}

impl From<DatabaseError> for AccountingStoreError {
    fn from(e: DatabaseError) -> Self {
        Self::Storage(e.to_string())
    }
}

/// Type-erased accounting store trait for use in `Accounting` without a DB generic.
pub trait AccountingStore: Send + Sync + fmt::Debug {
    /// Save a single peer's state.
    fn save(
        &self,
        peer: OverlayAddress,
        state: PeerAccounting,
    ) -> Result<(), AccountingStoreError>;

    /// Save a batch of peer states in a single transaction.
    fn save_batch(
        &self,
        entries: &[(OverlayAddress, PeerAccounting)],
    ) -> Result<(), AccountingStoreError>;

    /// Load a single peer's state.
    fn load(&self, peer: OverlayAddress) -> Result<Option<PeerAccounting>, AccountingStoreError>;

    /// Load all peer states.
    fn load_all(
        &self,
    ) -> Result<Vec<(OverlayAddress, PeerAccounting)>, AccountingStoreError>;

    /// Remove a peer's state.
    fn remove(&self, peer: OverlayAddress) -> Result<bool, AccountingStoreError>;
}

/// Database-backed accounting store using the `vertex-storage` `Database` trait.
///
/// Each operation is a single transaction.
pub struct DbAccountingStore<DB: Database> {
    db: Arc<DB>,
}

impl<DB: Database> fmt::Debug for DbAccountingStore<DB> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DbAccountingStore").finish()
    }
}

impl<DB: Database> DbAccountingStore<DB> {
    /// Create a new accounting store backed by the given database.
    pub fn new(db: Arc<DB>) -> Self {
        Self { db }
    }

    /// Initialise the accounting table (call once at startup).
    pub fn init(&self) -> Result<(), AccountingStoreError> {
        self.db
            .update(|tx| {
                tx.ensure_table(AccountingTable::NAME)?;
                Ok(())
            })
            .map_err(AccountingStoreError::from)
    }
}

impl<DB: Database> AccountingStore for DbAccountingStore<DB> {
    fn save(
        &self,
        peer: OverlayAddress,
        state: PeerAccounting,
    ) -> Result<(), AccountingStoreError> {
        self.db
            .update(|tx| {
                tx.put::<AccountingTable>(peer, state)?;
                Ok(())
            })
            .map_err(AccountingStoreError::from)
    }

    fn save_batch(
        &self,
        entries: &[(OverlayAddress, PeerAccounting)],
    ) -> Result<(), AccountingStoreError> {
        if entries.is_empty() {
            return Ok(());
        }
        self.db
            .update(|tx| {
                for (peer, state) in entries {
                    tx.put::<AccountingTable>(*peer, state.clone())?;
                }
                Ok(())
            })
            .map_err(AccountingStoreError::from)
    }

    fn load(
        &self,
        peer: OverlayAddress,
    ) -> Result<Option<PeerAccounting>, AccountingStoreError> {
        self.db
            .view(|tx| tx.get::<AccountingTable>(peer))
            .map_err(AccountingStoreError::from)
    }

    fn load_all(
        &self,
    ) -> Result<Vec<(OverlayAddress, PeerAccounting)>, AccountingStoreError> {
        self.db
            .view(|tx| tx.entries::<AccountingTable>())
            .map_err(AccountingStoreError::from)
    }

    fn remove(&self, peer: OverlayAddress) -> Result<bool, AccountingStoreError> {
        self.db
            .update(|tx| tx.delete::<AccountingTable>(peer))
            .map_err(AccountingStoreError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_db() -> Arc<vertex_storage_redb::RedbDatabase> {
        let db = vertex_storage_redb::RedbDatabase::in_memory().unwrap();
        db.into_arc()
    }

    fn make_state(balance: i64) -> PeerAccounting {
        let state = PeerAccounting::new(1000, 10000);
        state.add_balance(balance);
        state
    }

    fn make_peer(n: u8) -> OverlayAddress {
        OverlayAddress::from([n; 32])
    }

    #[test]
    fn test_save_and_load() {
        let db = setup_db();
        let store = DbAccountingStore::new(db);
        store.init().unwrap();

        let peer = make_peer(1);
        store.save(peer, make_state(500)).unwrap();

        let loaded = store.load(peer).unwrap().expect("should find peer");
        assert_eq!(loaded.balance(), 500);
        assert_eq!(loaded.credit_limit(), 1000);
    }

    #[test]
    fn test_load_missing() {
        let db = setup_db();
        let store = DbAccountingStore::new(db);
        store.init().unwrap();

        let result = store.load(make_peer(99)).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_save_batch_and_load_all() {
        let db = setup_db();
        let store = DbAccountingStore::new(db);
        store.init().unwrap();

        let entries: Vec<_> = (1..=5)
            .map(|n| (make_peer(n), make_state(n as i64 * 100)))
            .collect();

        store.save_batch(&entries).unwrap();

        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 5);
    }

    #[test]
    fn test_save_batch_empty() {
        let db = setup_db();
        let store = DbAccountingStore::new(db);
        store.init().unwrap();

        store.save_batch(&[]).unwrap();
        let all = store.load_all().unwrap();
        assert!(all.is_empty());
    }

    #[test]
    fn test_remove() {
        let db = setup_db();
        let store = DbAccountingStore::new(db);
        store.init().unwrap();

        let peer = make_peer(1);
        store.save(peer, make_state(100)).unwrap();
        assert!(store.load(peer).unwrap().is_some());

        let removed = store.remove(peer).unwrap();
        assert!(removed);
        assert!(store.load(peer).unwrap().is_none());

        // Removing again returns false.
        let removed = store.remove(peer).unwrap();
        assert!(!removed);
    }

    #[test]
    fn test_overwrite() {
        let db = setup_db();
        let store = DbAccountingStore::new(db);
        store.init().unwrap();

        let peer = make_peer(1);
        store.save(peer, make_state(100)).unwrap();
        store.save(peer, make_state(200)).unwrap();

        let loaded = store.load(peer).unwrap().unwrap();
        assert_eq!(loaded.balance(), 200);
    }
}
