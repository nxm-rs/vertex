//! Traits for peer identifiers and persistence.

use std::fmt::Debug;
use std::hash::Hash;

use auto_impl::auto_impl;
use serde::{Deserialize, Serialize};

use crate::error::StoreError;
use crate::record::PeerRecord;

/// Blanket-implemented for any type with Clone + Eq + Hash + Send + Sync + Debug + Serialize + Deserialize.
pub trait NetPeerId:
    Clone + Eq + Hash + Send + Sync + Debug + Serialize + for<'de> Deserialize<'de> + 'static
{
}

impl<T> NetPeerId for T where
    T: Clone + Eq + Hash + Send + Sync + Debug + Serialize + for<'de> Deserialize<'de> + 'static
{
}

/// Bounds for peer data types used in storage.
pub trait DataBounds:
    Clone + Debug + Default + Send + Sync + Serialize + for<'de> Deserialize<'de> + 'static
{
}

impl<T> DataBounds for T where
    T: Clone + Debug + Default + Send + Sync + Serialize + for<'de> Deserialize<'de> + 'static
{
}

/// Peer persistence trait with auto-impl for &, Box, Arc.
#[auto_impl(&, Box, Arc)]
pub trait NetPeerStore<Id: NetPeerId, Data: DataBounds = ()>: Send + Sync {
    /// Load all peer records.
    fn load_all(&self) -> Result<Vec<PeerRecord<Id, Data>>, StoreError>;

    /// Save a peer record (insert or update).
    fn save(&self, record: &PeerRecord<Id, Data>) -> Result<(), StoreError>;

    /// Save multiple peer records.
    fn save_batch(&self, records: &[PeerRecord<Id, Data>]) -> Result<(), StoreError> {
        for record in records {
            self.save(record)?;
        }
        Ok(())
    }

    /// Remove a peer by ID. Returns true if a record was removed.
    fn remove(&self, id: &Id) -> Result<bool, StoreError>;

    /// Get a peer record by ID.
    fn get(&self, id: &Id) -> Result<Option<PeerRecord<Id, Data>>, StoreError>;

    /// Check if a peer exists.
    fn contains(&self, id: &Id) -> Result<bool, StoreError> {
        Ok(self.get(id)?.is_some())
    }

    /// Count stored peers.
    fn count(&self) -> Result<usize, StoreError>;

    /// Remove all peers.
    fn clear(&self) -> Result<(), StoreError>;

    /// Flush pending writes to storage (no-op for synchronous stores).
    fn flush(&self) -> Result<(), StoreError> {
        Ok(())
    }
}
