//! Database-backed peer store implementing NetPeerStore.

use std::sync::Arc;

use alloy_primitives::Address;
use vertex_net_peer_store::{NetPeerStore, StoreError};
use vertex_storage::{Database, DatabaseError, DbTx, DbTxMut, IndexedRead, IndexedWrite, Table, index, table};
use vertex_swarm_api::SwarmScoreStore;
use vertex_swarm_primitives::OverlayAddress;

use vertex_swarm_peer_score::PeerScore;

use crate::entry::StoredPeer;

// Primary table: OverlayAddress → StoredPeer
table!(pub(crate) PeerTable, "peers", OverlayAddress, StoredPeer);

// Secondary index: Ethereum address → OverlayAddress
index!(pub(crate) EthAddrPeerIndex, "peers_by_eth", Address, PeerTable, |peer| *peer.peer.ethereum_address());

// Score table: OverlayAddress → PeerScore
table!(pub(crate) ScoreTable, "peer_scores", OverlayAddress, PeerScore);

fn db_err(e: DatabaseError) -> StoreError {
    StoreError::Storage(e.to_string())
}

/// Database-backed peer store using the vertex-storage `Database` trait.
///
/// Maintains a secondary index on Ethereum address for cross-identity lookups.
/// Each operation is a single transaction.
pub struct DbPeerStore<DB: Database> {
    db: Arc<DB>,
}

impl<DB: Database> DbPeerStore<DB> {
    pub fn new(db: Arc<DB>) -> Self {
        Self { db }
    }

    /// Initialize the peers table, index, and score table (call once at startup).
    pub fn init(&self) -> Result<(), StoreError> {
        self.db.update(|tx| {
            tx.ensure_table(PeerTable::NAME)?;
            tx.ensure_table(EthAddrPeerIndex::NAME)?;
            tx.ensure_table(ScoreTable::NAME)?;
            Ok(())
        }).map_err(db_err)
    }

    /// Look up a peer by Ethereum address via the secondary index.
    pub fn get_by_eth_address(&self, addr: &Address) -> Result<Option<StoredPeer>, StoreError> {
        self.db.view(|tx| tx.get_via::<EthAddrPeerIndex>(*addr)).map_err(db_err)
    }

    /// Load all overlay addresses (key-only scan, no value deserialization).
    pub fn load_overlay_set(&self) -> Result<std::collections::HashSet<OverlayAddress>, StoreError> {
        self.db.view(|tx| {
            let keys = tx.keys::<PeerTable>()?;
            Ok(keys.into_iter().collect())
        }).map_err(db_err)
    }

    /// Batch-load specific peers by overlay address.
    pub fn load_batch(&self, ids: &[OverlayAddress]) -> Result<Vec<StoredPeer>, StoreError> {
        self.db.view(|tx| {
            let mut result = Vec::with_capacity(ids.len());
            for id in ids {
                if let Some(record) = tx.get::<PeerTable>(*id)? {
                    result.push(record);
                }
            }
            Ok(result)
        }).map_err(db_err)
    }

    /// Batch-remove peers by overlay address. Returns count of removed entries.
    pub fn remove_batch(&self, ids: &[OverlayAddress]) -> Result<usize, StoreError> {
        self.db.update(|tx| {
            let mut removed = 0;
            for id in ids {
                let _ = tx.delete::<ScoreTable>(*id);
                if tx.delete_indexed::<EthAddrPeerIndex>(*id)? {
                    removed += 1;
                }
            }
            Ok(removed)
        }).map_err(db_err)
    }
}

impl<DB: Database> NetPeerStore<StoredPeer> for DbPeerStore<DB> {
    fn load_all(&self) -> Result<Vec<StoredPeer>, StoreError> {
        self.db.view(|tx| {
            let entries = tx.entries::<PeerTable>()?;
            Ok(entries.into_iter().map(|(_, v)| v).collect())
        }).map_err(db_err)
    }

    fn load_ids(&self) -> Result<Vec<OverlayAddress>, StoreError> {
        self.db.view(|tx| tx.keys::<PeerTable>()).map_err(db_err)
    }

    fn save(&self, record: &StoredPeer) -> Result<(), StoreError> {
        let key = *record.peer.overlay();
        let value = record.clone();
        self.db.update(|tx| {
            tx.put_indexed::<EthAddrPeerIndex>(key, value)?;
            Ok(())
        }).map_err(db_err)
    }

    fn save_batch(&self, records: &[StoredPeer]) -> Result<(), StoreError> {
        self.db.update(|tx| {
            for record in records {
                let key = *record.peer.overlay();
                tx.put_indexed::<EthAddrPeerIndex>(key, record.clone())?;
            }
            Ok(())
        }).map_err(db_err)
    }

    fn remove(&self, id: &<StoredPeer as vertex_net_peer_store::NetRecord>::Id) -> Result<bool, StoreError> {
        self.db.update(|tx| {
            let _ = tx.delete::<ScoreTable>(*id);
            tx.delete_indexed::<EthAddrPeerIndex>(*id)
        }).map_err(db_err)
    }

    fn get(&self, id: &<StoredPeer as vertex_net_peer_store::NetRecord>::Id) -> Result<Option<StoredPeer>, StoreError> {
        self.db.view(|tx| tx.get::<PeerTable>(*id)).map_err(db_err)
    }

    fn contains(&self, id: &<StoredPeer as vertex_net_peer_store::NetRecord>::Id) -> Result<bool, StoreError> {
        self.db.view(|tx| Ok(tx.get::<PeerTable>(*id)?.is_some())).map_err(db_err)
    }

    fn count(&self) -> Result<usize, StoreError> {
        self.db.view(|tx| tx.count::<PeerTable>()).map_err(db_err)
    }

    fn clear(&self) -> Result<(), StoreError> {
        self.db.update(|tx| {
            tx.clear::<ScoreTable>()?;
            tx.clear_indexed::<EthAddrPeerIndex>()
        }).map_err(db_err)
    }

    fn flush(&self) -> Result<(), StoreError> {
        // redb commits are durable — no-op.
        Ok(())
    }
}

impl<DB: Database> SwarmScoreStore for DbPeerStore<DB> {
    type Score = PeerScore;
    type Error = StoreError;

    fn get_score(&self, overlay: &OverlayAddress) -> Result<Option<PeerScore>, StoreError> {
        self.db.view(|tx| tx.get::<ScoreTable>(*overlay)).map_err(db_err)
    }

    fn save_score_batch(&self, scores: &[(OverlayAddress, PeerScore)]) -> Result<(), StoreError> {
        if scores.is_empty() {
            return Ok(());
        }
        self.db.update(|tx| {
            for (overlay, score) in scores {
                tx.put::<ScoreTable>(*overlay, score.clone())?;
            }
            Ok(())
        }).map_err(db_err)
    }

    fn load_banned_overlays(&self) -> Result<Vec<OverlayAddress>, StoreError> {
        self.db.view(|tx| {
            let entries = tx.entries::<PeerTable>()?;
            Ok(entries.into_iter()
                .filter(|(_, v)| v.is_banned())
                .map(|(k, _)| k)
                .collect())
        }).map_err(db_err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::B256;
    use vertex_net_peer_store::NetRecord;
    use vertex_swarm_peer::SwarmPeer;
    use vertex_swarm_primitives::SwarmNodeType;

    fn make_stored_peer(n: u8) -> StoredPeer {
        let peer = vertex_swarm_test_utils::test_swarm_peer(n);
        StoredPeer {
            peer,
            node_type: SwarmNodeType::Client,
            ban_info: None,
            first_seen: 1000,
            last_seen: 2000,
            last_dial_attempt: 0,
            consecutive_failures: 0,
        }
    }

    /// Create a peer with a distinct Ethereum address (test_swarm_peer uses Address::ZERO).
    fn make_peer_with_eth(n: u8) -> StoredPeer {
        let peer_id = vertex_swarm_test_utils::test_peer_id(n);
        let multiaddrs = vec![format!("/ip4/127.0.0.{n}/tcp/1634/p2p/{peer_id}")
            .parse()
            .expect("valid multiaddr")];
        let peer = SwarmPeer::from_validated(
            multiaddrs,
            alloy_primitives::Signature::test_signature(),
            B256::repeat_byte(n),
            B256::ZERO,
            Address::repeat_byte(n),
        );
        StoredPeer {
            peer,
            node_type: SwarmNodeType::Client,
            ban_info: None,
            first_seen: 1000,
            last_seen: 2000,
            last_dial_attempt: 0,
            consecutive_failures: 0,
        }
    }

    fn setup_db() -> Arc<vertex_storage_redb::RedbDatabase> {
        let db = vertex_storage_redb::RedbDatabase::in_memory().unwrap();
        db.into_arc()
    }

    #[test]
    fn test_db_peer_store_crud() {
        let db = setup_db();
        let store = DbPeerStore::new(db);
        store.init().unwrap();

        let peer = make_stored_peer(1);
        let id = *peer.id();

        // Save
        store.save(&peer).unwrap();
        assert_eq!(store.count().unwrap(), 1);
        assert!(store.contains(&id).unwrap());

        // Get
        let loaded = store.get(&id).unwrap().unwrap();
        assert_eq!(loaded.node_type, peer.node_type);
        assert_eq!(loaded.first_seen, peer.first_seen);

        // Remove
        assert!(store.remove(&id).unwrap());
        assert_eq!(store.count().unwrap(), 0);
        assert!(!store.contains(&id).unwrap());
    }

    #[test]
    fn test_db_peer_store_batch() {
        let db = setup_db();
        let store = DbPeerStore::new(db);
        store.init().unwrap();

        let peers: Vec<_> = (1..=5).map(make_stored_peer).collect();
        store.save_batch(&peers).unwrap();
        assert_eq!(store.count().unwrap(), 5);

        let loaded = store.load_all().unwrap();
        assert_eq!(loaded.len(), 5);
    }

    #[test]
    fn test_db_peer_store_clear() {
        let db = setup_db();
        let store = DbPeerStore::new(db);
        store.init().unwrap();

        let peers: Vec<_> = (1..=3).map(make_stored_peer).collect();
        store.save_batch(&peers).unwrap();
        assert_eq!(store.count().unwrap(), 3);

        store.clear().unwrap();
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn test_load_overlay_set() {
        let db = setup_db();
        let store = DbPeerStore::new(db);
        store.init().unwrap();

        let peers: Vec<_> = (1..=5).map(make_stored_peer).collect();
        store.save_batch(&peers).unwrap();

        let overlays = store.load_overlay_set().unwrap();
        assert_eq!(overlays.len(), 5);
        for p in &peers {
            assert!(overlays.contains(p.id()));
        }
    }

    #[test]
    fn test_load_batch() {
        let db = setup_db();
        let store = DbPeerStore::new(db);
        store.init().unwrap();

        let peers: Vec<_> = (1..=5).map(make_stored_peer).collect();
        store.save_batch(&peers).unwrap();

        let ids: Vec<_> = peers[0..3].iter().map(|p| *p.id()).collect();
        let loaded = store.load_batch(&ids).unwrap();
        assert_eq!(loaded.len(), 3);

        // Loading non-existent IDs should just skip them
        let fake_id = vertex_swarm_test_utils::test_overlay(99);
        let loaded = store.load_batch(&[fake_id]).unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn test_remove_batch() {
        let db = setup_db();
        let store = DbPeerStore::new(db);
        store.init().unwrap();

        let peers: Vec<_> = (1..=5).map(make_stored_peer).collect();
        store.save_batch(&peers).unwrap();

        let ids: Vec<_> = peers[0..2].iter().map(|p| *p.id()).collect();
        let removed = store.remove_batch(&ids).unwrap();
        assert_eq!(removed, 2);
        assert_eq!(store.count().unwrap(), 3);

        // Removing already-removed should report 0
        let removed = store.remove_batch(&ids).unwrap();
        assert_eq!(removed, 0);
    }

    #[test]
    fn test_db_peer_store_overwrite() {
        let db = setup_db();
        let store = DbPeerStore::new(db);
        store.init().unwrap();

        let mut peer = make_stored_peer(1);
        store.save(&peer).unwrap();

        peer.node_type = SwarmNodeType::Storer;
        peer.consecutive_failures = 5;
        store.save(&peer).unwrap();

        assert_eq!(store.count().unwrap(), 1);
        let loaded = store.get(peer.id()).unwrap().unwrap();
        assert_eq!(loaded.node_type, SwarmNodeType::Storer);
        assert_eq!(loaded.consecutive_failures, 5);
    }

    #[test]
    fn test_get_by_eth_address() {
        let db = setup_db();
        let store = DbPeerStore::new(db);
        store.init().unwrap();

        let peer = make_peer_with_eth(1);
        store.save(&peer).unwrap();

        // Look up by eth address
        let found = store.get_by_eth_address(&Address::repeat_byte(1)).unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().peer.overlay(), peer.peer.overlay());

        // Non-existent eth address
        let missing = store.get_by_eth_address(&Address::repeat_byte(99)).unwrap();
        assert!(missing.is_none());
    }

    #[test]
    fn test_eth_index_multiple_peers() {
        let db = setup_db();
        let store = DbPeerStore::new(db);
        store.init().unwrap();

        let peers: Vec<_> = (1..=3).map(make_peer_with_eth).collect();
        store.save_batch(&peers).unwrap();

        // Each eth address resolves to the correct peer
        for peer in &peers {
            let found = store.get_by_eth_address(peer.peer.ethereum_address()).unwrap().unwrap();
            assert_eq!(found.peer.overlay(), peer.peer.overlay());
        }
    }

    #[test]
    fn test_eth_index_survives_remove() {
        let db = setup_db();
        let store = DbPeerStore::new(db);
        store.init().unwrap();

        let peer = make_peer_with_eth(1);
        let eth = *peer.peer.ethereum_address();
        store.save(&peer).unwrap();

        store.remove(peer.id()).unwrap();

        // Index entry should also be gone
        let found = store.get_by_eth_address(&eth).unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn test_eth_index_survives_clear() {
        let db = setup_db();
        let store = DbPeerStore::new(db);
        store.init().unwrap();

        let peers: Vec<_> = (1..=3).map(make_peer_with_eth).collect();
        store.save_batch(&peers).unwrap();

        store.clear().unwrap();

        for peer in &peers {
            let found = store.get_by_eth_address(peer.peer.ethereum_address()).unwrap();
            assert!(found.is_none());
        }
    }

    #[test]
    fn test_overlay_address_encode_decode() {
        use vertex_storage::{Decode, Encode};

        let addr = OverlayAddress::from([0xABu8; 32]);
        let encoded = addr.encode();
        assert_eq!(encoded.len(), 32);
        let decoded = OverlayAddress::decode(&encoded).unwrap();
        assert_eq!(decoded, addr);
    }

    #[test]
    fn test_stored_peer_serialize_deserialize() {
        let peer = vertex_swarm_test_utils::test_swarm_peer(1);
        let record = StoredPeer {
            peer,
            node_type: SwarmNodeType::Storer,
            ban_info: Some((100, "test ban".into())),
            first_seen: 1000,
            last_seen: 2000,
            last_dial_attempt: 1500,
            consecutive_failures: 3,
        };

        let serialized = postcard::to_allocvec(&record).expect("serialize");
        let deserialized: StoredPeer = postcard::from_bytes(&serialized).expect("deserialize");
        assert_eq!(deserialized.node_type, record.node_type);
        assert_eq!(deserialized.first_seen, record.first_seen);
        assert_eq!(deserialized.last_seen, record.last_seen);
        assert_eq!(deserialized.consecutive_failures, record.consecutive_failures);
        assert!(deserialized.ban_info.is_some());
    }
}
