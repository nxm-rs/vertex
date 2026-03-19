//! Peer persistence with generic record storage.

use std::fmt::Debug;
use std::hash::Hash;
use std::path::PathBuf;

use auto_impl::auto_impl;
use serde::{Deserialize, Serialize};

/// Errors from peer store operations.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("failed to create directory {}: {source}", path.display())]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to open {}: {source}", path.display())]
    Open {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to read {}: {source}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to write {}: {source}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to deserialize {}: {reason}", path.display())]
    Deserialize { path: PathBuf, reason: String },

    #[error("failed to serialize {}: {reason}", path.display())]
    Serialize { path: PathBuf, reason: String },

    #[error("storage error: {0}")]
    Storage(String),
}

impl StoreError {
    /// Get the path associated with this error, if any.
    pub fn path(&self) -> Option<&PathBuf> {
        match self {
            Self::CreateDir { path, .. }
            | Self::Open { path, .. }
            | Self::Read { path, .. }
            | Self::Write { path, .. }
            | Self::Deserialize { path, .. }
            | Self::Serialize { path, .. } => Some(path),
            Self::Storage(_) => None,
        }
    }
}

/// Peer identifier type.
pub trait NetPeerId:
    Clone + Eq + Hash + Send + Sync + Debug + Serialize + for<'de> Deserialize<'de> + 'static
{
}

impl<T> NetPeerId for T where
    T: Clone + Eq + Hash + Send + Sync + Debug + Serialize + for<'de> Deserialize<'de> + 'static
{
}

/// Serializable peer record with an associated ID.
pub trait NetRecord:
    Clone + Debug + Send + Sync + Serialize + for<'de> Deserialize<'de> + 'static
{
    type Id: NetPeerId;
    fn id(&self) -> &Self::Id;
}

/// Peer persistence trait with auto-impl for &, Box, Arc.
#[auto_impl(&, Box, Arc)]
pub trait NetPeerStore<R: NetRecord>: Send + Sync {
    /// Load all peer records.
    fn load_all(&self) -> Result<Vec<R>, StoreError>;

    /// Load only the IDs of all stored records (no value deserialization).
    fn load_ids(&self) -> Result<Vec<R::Id>, StoreError> {
        Ok(self
            .load_all()?
            .into_iter()
            .map(|r| r.id().clone())
            .collect())
    }

    /// Save a peer record (insert or update).
    fn save(&self, record: &R) -> Result<(), StoreError>;

    /// Save multiple peer records.
    fn save_batch(&self, records: &[R]) -> Result<(), StoreError> {
        for record in records {
            self.save(record)?;
        }
        Ok(())
    }

    /// Remove a peer by ID. Returns true if a record was removed.
    fn remove(&self, id: &R::Id) -> Result<bool, StoreError>;

    /// Get a peer record by ID.
    fn get(&self, id: &R::Id) -> Result<Option<R>, StoreError>;

    fn contains(&self, id: &R::Id) -> Result<bool, StoreError> {
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

#[cfg(any(test, feature = "test-utils"))]
mod memory;

#[cfg(any(test, feature = "test-utils"))]
pub use memory::MemoryPeerStore;
