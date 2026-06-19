//! Storer node storage backend: the persisting chunk store and reserve.
//!
//! This crate provides the storage primitives for Storer nodes:
//! - [`ChunkStore`] - Backend trait for chunk persistence
//! - [`DbChunkStore`] - Implementation over the vertex-storage `Database` trait
//! - [`Reserve`] - Capacity management and garbage collection
//!
//! The storer's `SwarmLocalStore` implementation (a persisting reserve that
//! stores chunk and stamp as separate fields, evicts furthest-from-neighbourhood,
//! and signs receipts on pushsync) is built on top of these primitives. It is
//! tracked separately from the cache-only client and is not part of this crate
//! yet.
//!
//! # Architecture
//!
//! ```text
//! DbChunkStore
//! └── store: ChunkStore (Database-trait backend)
//!
//! Reserve
//! ├── capacity: u64 (max chunks)
//! ├── used: u64 (current count)
//! └── eviction_strategy: EvictionStrategy
//! ```

#![cfg_attr(not(feature = "std"), no_std)]

mod cache;
mod db_reserve;
mod db_store;
mod error;
mod reserve;
mod traits;

pub use cache::ChunkCache;
pub use db_reserve::DbReserve;
pub use db_store::DbChunkStore;
pub use error::StorerError;
pub use reserve::{EvictionStrategy, Reserve};
pub use traits::ChunkStore;

/// Result type for storer operations.
pub type StorerResult<T> = Result<T, StorerError>;
