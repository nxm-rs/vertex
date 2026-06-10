//! Storer node storage implementation with LocalStore and ChunkSync.
//!
//! This crate provides the storage layer for Storer nodes:
//! - [`ChunkStore`] - Backend trait for chunk persistence
//! - [`DbChunkStore`] - Implementation over the vertex-storage `Database` trait
//! - [`LocalStoreImpl`] - Implements [`LocalStore`] from swarm-api
//! - [`Reserve`] - Capacity management and garbage collection
//!
//! # Architecture
//!
//! ```text
//! LocalStoreImpl
//! ├── store: ChunkStore (Database-trait backend)
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
//! use vertex_swarm_storer::{DbChunkStore, LocalStoreImpl, Reserve};
//!
//! // Create a chunk store over the node's shared database handle
//! let chunk_store = DbChunkStore::new(db.clone())?;
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
mod db_store;
mod error;
mod reserve;
mod store;
mod traits;

pub use cache::ChunkCache;
pub use db_store::DbChunkStore;
pub use error::StorerError;
pub use reserve::{EvictionStrategy, Reserve};
pub use store::LocalStoreImpl;
pub use traits::ChunkStore;

/// Result type for storer operations.
pub type StorerResult<T> = Result<T, StorerError>;
