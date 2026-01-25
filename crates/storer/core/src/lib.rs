//! Full node storage implementation with LocalStore and ChunkSync.
//!
//! This crate provides the storage layer for full nodes:
//! - [`ChunkStore`] - Backend trait for chunk persistence
//! - [`RedbChunkStore`] - redb-based implementation
//! - [`LocalStoreImpl`] - Implements [`LocalStore`] from swarm-api
//! - [`Reserve`] - Capacity management and garbage collection
//!
//! # Architecture
//!
//! ```text
//! LocalStoreImpl
//! ├── store: ChunkStore (redb backend)
//! ├── cache: LRU cache for hot chunks
//! └── reserve: Reserve (capacity tracking)
//!
//! Reserve
//! ├── capacity: u64 (max chunks)
//! ├── used: u64 (current count)
//! └── eviction_strategy: EvictionStrategy
//! ```
//!
//! # Usage
//!
//! ```ignore
//! use vertex_storer_core::{RedbChunkStore, LocalStoreImpl, Reserve};
//!
//! // Create redb-backed store
//! let chunk_store = RedbChunkStore::open("./chunks.redb")?;
//! let reserve = Reserve::new(1_000_000); // 1M chunks
//!
//! // Create LocalStore implementation
//! let local_store = LocalStoreImpl::new(chunk_store, reserve);
//!
//! // Store and retrieve chunks
//! local_store.store(&chunk)?;
//! let retrieved = local_store.retrieve(&address)?;
//! ```

#![cfg_attr(not(feature = "std"), no_std)]

mod cache;
mod error;
mod redb_store;
mod reserve;
mod store;
mod traits;

pub use cache::ChunkCache;
pub use error::StorerError;
pub use redb_store::RedbChunkStore;
pub use reserve::{EvictionStrategy, Reserve};
pub use store::LocalStoreImpl;
pub use traits::ChunkStore;

/// Result type for storer operations.
pub type StorerResult<T> = Result<T, StorerError>;
