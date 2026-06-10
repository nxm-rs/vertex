//! Identity-only peer snapshot persistence.
//!
//! The peer set lives entirely in memory; persistence is a periodic
//! whole-set snapshot plus a single load at startup. There is no
//! random-access record store: a crash loses at most one snapshot
//! interval of newly discovered peers, which rediscovery via bootnodes
//! and gossip replaces in seconds.

pub mod error;

use auto_impl::auto_impl;
use error::StoreError;

/// Whole-set snapshot persistence for peer records.
///
/// `load` runs once at startup to seed the in-memory peer set; `store`
/// replaces the persisted set wholesale (clear plus put, one transaction).
/// Implementations decide the backing medium (database table, memory).
#[auto_impl(&, Box, Arc)]
pub trait PeerSnapshotStore<R>: Send + Sync {
    /// Load the persisted snapshot. Called once, at startup.
    fn load(&self) -> Result<Vec<R>, StoreError>;

    /// Replace the persisted snapshot with `records` in one transaction.
    fn store(&self, records: &[R]) -> Result<(), StoreError>;
}

#[cfg(any(test, feature = "test-utils"))]
mod memory;

#[cfg(any(test, feature = "test-utils"))]
pub use memory::MemoryPeerStore;
