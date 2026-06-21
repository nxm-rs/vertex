//! Persisting [`IntervalStore`] for the pull-sync service over the
//! vertex-storage `Database`.
//!
//! Two tables track per-peer sync progress: a `(peer, bin) -> binid` interval
//! table and a `peer -> epoch` table. The compound interval key is big-endian
//! `[peer: 32][bin: 1]`, so a peer's bins are a contiguous, bin-ascending range.

use std::sync::Arc;

use nectar_primitives::Bin;
use vertex_storage::{Database, DatabaseError, DbTx, DbTxMut, Decode, Encode, Table, table};
use vertex_swarm_api::{IntervalStore, SwarmError, SwarmResult};
use vertex_swarm_primitives::OverlayAddress;

// Per-(peer, bin) interval cursor: the last synced insertion sequence.
table!(pub(crate) Interval, "puller_interval", PeerBinKey, u64, compressed = false);

// Per-peer reserve epoch last seen, to detect a reserve wipe at the source.
table!(pub(crate) PeerEpoch, "puller_peer_epoch", OverlayAddress, u64, compressed = false);

/// Compound key `(peer, bin)` for [`Interval`].
///
/// Big-endian `[peer: 32][bin: 1]` (33 bytes): a peer's bins are contiguous and
/// ascending by bin.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub(crate) struct PeerBinKey {
    pub(crate) peer: OverlayAddress,
    pub(crate) bin: u8,
}

impl PeerBinKey {
    fn new(peer: OverlayAddress, bin: Bin) -> Self {
        Self {
            peer,
            bin: bin.get(),
        }
    }
}

impl Encode for PeerBinKey {
    type Encoded = [u8; 33];

    fn encode(self) -> Self::Encoded {
        let mut out = [0u8; 33];
        out[..32].copy_from_slice(self.peer.as_slice());
        out[32] = self.bin;
        out
    }
}

impl Decode for PeerBinKey {
    fn decode(value: &[u8]) -> Result<Self, DatabaseError> {
        let bytes: [u8; 33] = value.try_into().map_err(|_| DatabaseError::Decode)?;
        let peer: [u8; 32] = bytes[..32].try_into().map_err(|_| DatabaseError::Decode)?;
        Ok(Self {
            peer: OverlayAddress::from(peer),
            bin: bytes[32],
        })
    }
}

/// Persisting interval store backing the pull-sync resume point.
pub struct DbIntervalStore<DB: Database> {
    db: Arc<DB>,
}

impl<DB: Database> DbIntervalStore<DB> {
    /// Open the interval store, ensuring both tables exist.
    pub fn new(db: Arc<DB>) -> Result<Self, DatabaseError> {
        db.update(|tx| {
            tx.ensure_table(Interval::NAME)?;
            tx.ensure_table(PeerEpoch::NAME)
        })?;
        Ok(Self { db })
    }
}

fn storage_err(err: DatabaseError) -> SwarmError {
    SwarmError::storage(err)
}

impl<DB: Database> IntervalStore for DbIntervalStore<DB> {
    fn interval(&self, peer: &OverlayAddress, bin: Bin) -> SwarmResult<u64> {
        Ok(self
            .db
            .view(|tx| tx.get::<Interval>(PeerBinKey::new(*peer, bin)))
            .map_err(storage_err)?
            .unwrap_or(0))
    }

    fn set_interval(&self, peer: &OverlayAddress, bin: Bin, binid: u64) -> SwarmResult<()> {
        self.db
            .update(|tx| tx.put::<Interval>(PeerBinKey::new(*peer, bin), binid))
            .map_err(storage_err)
    }

    fn peer_epoch(&self, peer: &OverlayAddress) -> SwarmResult<Option<u64>> {
        self.db
            .view(|tx| tx.get::<PeerEpoch>(*peer))
            .map_err(storage_err)
    }

    fn set_peer_epoch(&self, peer: &OverlayAddress, epoch: u64) -> SwarmResult<()> {
        self.db
            .update(|tx| tx.put::<PeerEpoch>(*peer, epoch))
            .map_err(storage_err)
    }

    fn reset_peer(&self, peer: &OverlayAddress, epoch: u64) -> SwarmResult<()> {
        self.db
            .update(|tx| {
                // The `[peer: 32][bin: 1]` key makes this peer's bins the contiguous
                // run `[peer][0x00..=0xFF]`, so deleting that range clears exactly
                // its intervals and never bleeds into an adjacent peer. Absent keys
                // already read as 0, so a delete is equivalent to zeroing.
                for bin in 0u8..=u8::MAX {
                    tx.delete::<Interval>(PeerBinKey { peer: *peer, bin })?;
                }
                // Epoch last and in the same tx: it is the commit barrier the puller
                // checks, so it must never become visible before the interval clear.
                tx.put::<PeerEpoch>(*peer, epoch)
            })
            .map_err(storage_err)
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "test assertions over known-bounds fixtures"
)]
mod tests {
    use super::*;
    use vertex_storage_redb::RedbDatabase;

    fn peer(n: u8) -> OverlayAddress {
        OverlayAddress::from([n; 32])
    }

    fn bin(n: u8) -> Bin {
        Bin::new(n).unwrap()
    }

    fn store() -> DbIntervalStore<RedbDatabase> {
        DbIntervalStore::new(RedbDatabase::in_memory().unwrap().into_arc()).unwrap()
    }

    #[test]
    fn unseen_interval_is_zero() {
        let s = store();
        assert_eq!(s.interval(&peer(1), bin(3)).unwrap(), 0);
    }

    #[test]
    fn interval_round_trips_per_peer_and_bin() {
        let s = store();
        s.set_interval(&peer(1), bin(3), 42).unwrap();
        s.set_interval(&peer(1), bin(4), 7).unwrap();
        s.set_interval(&peer(2), bin(3), 99).unwrap();

        assert_eq!(s.interval(&peer(1), bin(3)).unwrap(), 42);
        assert_eq!(s.interval(&peer(1), bin(4)).unwrap(), 7);
        assert_eq!(s.interval(&peer(2), bin(3)).unwrap(), 99);
        // A bin never written stays at zero for that peer.
        assert_eq!(s.interval(&peer(2), bin(4)).unwrap(), 0);
    }

    #[test]
    fn peer_epoch_round_trips() {
        let s = store();
        assert_eq!(s.peer_epoch(&peer(1)).unwrap(), None);
        s.set_peer_epoch(&peer(1), 5).unwrap();
        assert_eq!(s.peer_epoch(&peer(1)).unwrap(), Some(5));
        s.set_peer_epoch(&peer(1), 6).unwrap();
        assert_eq!(s.peer_epoch(&peer(1)).unwrap(), Some(6));
    }

    #[test]
    fn intervals_survive_reopen() {
        let db = RedbDatabase::in_memory().unwrap().into_arc();
        DbIntervalStore::new(Arc::clone(&db))
            .unwrap()
            .set_interval(&peer(1), bin(3), 42)
            .unwrap();
        let reopened = DbIntervalStore::new(db).unwrap();
        assert_eq!(reopened.interval(&peer(1), bin(3)).unwrap(), 42);
    }

    #[test]
    fn reset_peer_clears_all_bins_sets_epoch_and_spares_neighbours() {
        let s = store();

        // Two peers whose 32-byte overlays are adjacent (differ only in the last
        // byte), so their `[peer][bin]` key ranges immediately abut: a range
        // delete that bled past the 32-byte boundary would corrupt the neighbour.
        let target = OverlayAddress::from([0u8; 32]);
        let mut neighbour_bytes = [0u8; 32];
        neighbour_bytes[31] = 1;
        let neighbour = OverlayAddress::from(neighbour_bytes);

        for b in [bin(0), bin(3), bin(31)] {
            s.set_interval(&target, b, 100).unwrap();
            s.set_interval(&neighbour, b, 200).unwrap();
        }
        s.set_peer_epoch(&target, 5).unwrap();

        s.reset_peer(&target, 9).unwrap();

        // Every bin for the target is back to zero and the new epoch is recorded.
        for b in [bin(0), bin(3), bin(31)] {
            assert_eq!(s.interval(&target, b).unwrap(), 0);
        }
        assert_eq!(s.peer_epoch(&target).unwrap(), Some(9));

        // The boundary-adjacent neighbour is untouched.
        for b in [bin(0), bin(3), bin(31)] {
            assert_eq!(s.interval(&neighbour, b).unwrap(), 200);
        }
    }
}
