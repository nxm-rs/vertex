//! Storer node storage backend: the persisting chunk store and reserve.
//!
//! Storage primitives for Storer nodes: [`ChunkStore`]/[`DbChunkStore`] for
//! chunk persistence over the vertex-storage `Database` trait, and [`Reserve`]
//! for capacity management and eviction. The persisting `SwarmLocalStore`
//! reserve is built on top of these.

#![cfg_attr(not(feature = "std"), no_std)]

mod cache;
mod db_reserve;
mod db_store;
mod error;
mod expiry;
mod radius;
mod reserve;
mod traits;

pub use cache::ChunkCache;
pub use db_reserve::DbReserve;
pub use db_store::DbChunkStore;
pub use error::StorerError;
pub use expiry::{EVICT_BATCH_MAX, ExpirySweep, SweepReport, expired_batches};
pub use radius::{
    BIN_EVICT_MAX, RadiusController, RadiusDecision, RadiusOutcome, ReserveOccupancy,
    derive_radius, grow_to_capacity, occupancy_of, shrink_threshold,
};
pub use reserve::{EvictionStrategy, Reserve};
pub use traits::ChunkStore;

/// Result type for storer operations.
pub type StorerResult<T> = Result<T, StorerError>;
