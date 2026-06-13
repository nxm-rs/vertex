//! Generic in-memory stores reusable beyond chunks.
//!
//! This crate owns the type-generalised, byte-bounded LRU used by the Swarm
//! client cache. It is deliberately domain-agnostic: it knows nothing about
//! chunks, addresses, or postage stamps, so it can serve any consumer that
//! needs a lossy memory budget over a key/value pair. The Swarm-specific
//! caching policy (content vs single-owner freshness) lives in the swarm
//! wrapper that holds one of these, not here.
//!
//! It is wasm-clean by construction: interior mutability is `parking_lot`,
//! there is no tokio, no IO, and no clock. A persisting reserve is a different
//! backend behind the same higher-level store trait and is not implemented
//! here.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]

mod bounded_lru;

pub use bounded_lru::{BoundedLruStore, ByteSized};
