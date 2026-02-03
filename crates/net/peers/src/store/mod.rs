//! Peer persistence trait and implementations (memory, file).

mod file;
mod memory;

use std::fmt::Debug;

use auto_impl::auto_impl;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::state::NetPeerSnapshot;
use crate::traits::NetPeerId;

pub use file::FilePeerStore;
pub use memory::MemoryPeerStore;

#[derive(Debug, Error)]
pub enum PeerStoreError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Serialization error: {0}")]
    Serialization(String),
    #[error("Storage error: {0}")]
    Storage(String),
}

/// Bounds for snapshot extension types used in storage.
pub trait ExtSnapBounds:
    Clone + Debug + Send + Sync + Serialize + for<'de> Deserialize<'de> + 'static
{
}

impl<T> ExtSnapBounds for T where
    T: Clone + Debug + Send + Sync + Serialize + for<'de> Deserialize<'de> + 'static
{
}

/// Peer persistence trait with auto-impl for &, Box, Arc.
///
/// Generic over `ExtSnap` for protocol-specific state extension snapshot,
/// and `ScoreExtSnap` for protocol-specific scoring extension snapshot.
#[auto_impl(&, Box, Arc)]
pub trait NetPeerStore<Id: NetPeerId, ExtSnap: ExtSnapBounds = (), ScoreExtSnap: ExtSnapBounds = ()>:
    Send + Sync
{
    fn load_all(&self) -> Result<Vec<NetPeerSnapshot<Id, ExtSnap, ScoreExtSnap>>, PeerStoreError>;
    fn save(
        &self,
        snapshot: &NetPeerSnapshot<Id, ExtSnap, ScoreExtSnap>,
    ) -> Result<(), PeerStoreError>;

    fn save_batch(
        &self,
        snapshots: &[NetPeerSnapshot<Id, ExtSnap, ScoreExtSnap>],
    ) -> Result<(), PeerStoreError> {
        for snapshot in snapshots {
            self.save(snapshot)?;
        }
        Ok(())
    }

    fn remove(&self, id: &Id) -> Result<(), PeerStoreError>;
    fn get(
        &self,
        id: &Id,
    ) -> Result<Option<NetPeerSnapshot<Id, ExtSnap, ScoreExtSnap>>, PeerStoreError>;

    fn contains(&self, id: &Id) -> Result<bool, PeerStoreError> {
        Ok(self.get(id)?.is_some())
    }

    fn count(&self) -> Result<usize, PeerStoreError>;
    fn clear(&self) -> Result<(), PeerStoreError>;

    fn flush(&self) -> Result<(), PeerStoreError> {
        Ok(())
    }
}
