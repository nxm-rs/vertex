//! Database-backed peer snapshot store.

use std::sync::Arc;

use vertex_net_peer_store::PeerSnapshotStore;
use vertex_net_peer_store::error::StoreError;
use vertex_storage::{Database, DatabaseError, DbTx, DbTxMut, Table, table};
use vertex_swarm_primitives::OverlayAddress;

use crate::entry::PeerSnapshot;

// Single snapshot table: OverlayAddress -> PeerSnapshot. The table name is
// new with the identity-only record; tables from earlier schemas are simply
// ignored.
table!(pub(crate) PeerSnapshotTable, "peer_snapshots", OverlayAddress, PeerSnapshot);

fn db_err(e: DatabaseError) -> StoreError {
    StoreError::Storage(e.to_string())
}

/// Peer snapshot store over the vertex-storage `Database` trait.
///
/// `store` replaces the whole table in one transaction; `load` reads it back
/// at startup. Generic over the database so non-redb backends (for example a
/// wasm-targeted store) can slot in unchanged.
pub struct DbPeerSnapshotStore<DB: Database> {
    db: Arc<DB>,
}

impl<DB: Database> DbPeerSnapshotStore<DB> {
    pub fn new(db: Arc<DB>) -> Self {
        Self { db }
    }

    /// Initialize the snapshot table (call once at startup).
    pub fn init(&self) -> Result<(), StoreError> {
        self.db
            .update(|tx| tx.ensure_table(PeerSnapshotTable::NAME))
            .map_err(db_err)
    }
}

impl<DB: Database> PeerSnapshotStore<PeerSnapshot> for DbPeerSnapshotStore<DB> {
    fn load(&self) -> Result<Vec<PeerSnapshot>, StoreError> {
        self.db
            .view(|tx| {
                let entries = tx.entries::<PeerSnapshotTable>()?;
                Ok(entries.into_iter().map(|(_, v)| v).collect())
            })
            .map_err(db_err)
    }

    fn store(&self, records: &[PeerSnapshot]) -> Result<(), StoreError> {
        self.db
            .update(|tx| {
                tx.clear::<PeerSnapshotTable>()?;
                for record in records {
                    tx.put::<PeerSnapshotTable>(*record.peer.overlay(), record.clone())?;
                }
                Ok(())
            })
            .map_err(db_err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use vertex_swarm_primitives::SwarmNodeType;

    fn make_snapshot(n: u8) -> PeerSnapshot {
        PeerSnapshot {
            peer: vertex_swarm_test_utils::test_swarm_peer(n),
            node_type: SwarmNodeType::Storer,
            last_seen: 1000 + n as u64,
        }
    }

    fn setup_store() -> DbPeerSnapshotStore<vertex_storage_redb::RedbDatabase> {
        let db = vertex_storage_redb::RedbDatabase::in_memory()
            .unwrap()
            .into_arc();
        let store = DbPeerSnapshotStore::new(db);
        store.init().unwrap();
        store
    }

    #[test]
    fn test_redb_roundtrip_identical_set() {
        let store = setup_store();

        let records: Vec<_> = (1..=5).map(make_snapshot).collect();
        store.store(&records).unwrap();

        let loaded = store.load().unwrap();
        assert_eq!(loaded.len(), 5);

        let stored_overlays: HashSet<_> = records.iter().map(|r| *r.peer.overlay()).collect();
        let loaded_overlays: HashSet<_> = loaded.iter().map(|r| *r.peer.overlay()).collect();
        assert_eq!(stored_overlays, loaded_overlays);
        for record in &loaded {
            assert_eq!(record.node_type, SwarmNodeType::Storer);
        }
    }

    #[test]
    fn test_store_is_full_replace() {
        let store = setup_store();

        store
            .store(&(1..=5).map(make_snapshot).collect::<Vec<_>>())
            .unwrap();
        store
            .store(&(6..=7).map(make_snapshot).collect::<Vec<_>>())
            .unwrap();

        let loaded = store.load().unwrap();
        assert_eq!(loaded.len(), 2);
        let overlays: HashSet<_> = loaded.iter().map(|r| *r.peer.overlay()).collect();
        assert!(overlays.contains(make_snapshot(6).peer.overlay()));
        assert!(overlays.contains(make_snapshot(7).peer.overlay()));
    }

    #[test]
    fn test_empty_store_clears() {
        let store = setup_store();
        store.store(&[make_snapshot(1)]).unwrap();
        store.store(&[]).unwrap();
        assert!(store.load().unwrap().is_empty());
    }

    #[test]
    fn test_load_on_fresh_store_is_empty() {
        let store = setup_store();
        assert!(store.load().unwrap().is_empty());
    }
}
