//! IndexedDB backend for `vertex-storage`, for the browser client cache.
//!
//! The [`Database`] trait is synchronous; IndexedDB is async. The backend is
//! therefore an in-memory write-through map that answers every read and write
//! synchronously and is the authoritative copy, mirrored to IndexedDB by a
//! fire-and-forget `spawn_local` task on each committed write. Durability is
//! best-effort, which is acceptable for the lossy client chunk cache: a dropped
//! mirror write costs at most a re-fetch after a restart, never correctness.
//!
//! [`Database`]: vertex_storage::Database

#![cfg(target_arch = "wasm32")]
// The `Database` trait surface returns `Result<Vec<(T::Key, T::Value)>>` shapes
// that clippy reads as complex; they mirror the trait and cannot be simplified.
#![allow(clippy::type_complexity)]

mod persist;
mod store;

pub use store::IndexedDbDatabase;
