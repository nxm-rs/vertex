//! Unified, configuration-driven Swarm contract indexer.
//!
//! One crate subsuming the five per-contract indexer crates. It plugs into the
//! generic [`vertex_chain_index`] engine as a SINGLE
//! [`ContractIndexer`](indexer::ContractIndexer): one combined
//! multi-address/multi-topic [`Filter`](alloy_rpc_types_eth::Filter), one cursor
//! (the engine's existing per-name cursor), and one position-keyed event store.
//!
//! # Shape
//!
//! - [`registry`]: contracts as data. A [`WatchedContract`] /
//!   [`EventDescriptor`] model and the static [`registry`](registry::registry)
//!   built from the canonical `nectar_contracts` address book. Adding a contract
//!   is one [`ContractId`] variant plus one registry entry.
//! - [`indexer`]: the one [`ContractIndexer`](indexer::ContractIndexer). `apply`
//!   is total and never decodes an event body: it resolves `log.address()` to a
//!   [`ContractId`], verifies the `topic0` is declared for that contract, caps
//!   the `data` size, and writes the row verbatim. Decoding is a view concern.
//! - [`store`]: the ONE generic [`EventTable`](store::EventTable) keyed by
//!   `(contract, block, log_index)`, plus the one materialized projection
//!   (postage value-sorted eviction) as a self-healing
//!   [`SecondaryIndex`](vertex_storage::SecondaryIndex).
//! - [`views`]: lazy readers that fold position-ordered rows and decode on read
//!   with the concrete nectar `sol!` types. The five typed projections of the
//!   branches become these pure functions.
//!
//! # Design
//!
//! This is a strict instance of `CHAIN_REACTIONS_DESIGN.md`: `apply` is a pure
//! idempotent fold into the store with no hooks; every derived value is computed
//! lazily on read; the one materialized index is an ordering HINT carrying no
//! decision, recomputed at dequeue. Idempotency is structural (the
//! `(contract, block, log_index)` position IS the key, so a replayed range
//! overwrites each row in place), so there is no per-domain supersede guard.
//!
//! # Security surface
//!
//! Generalizing concentrates several previously per-crate risks into one store
//! and one `apply`. The defences:
//!
//! - **Unknown / malformed event decode.** `apply` never decodes the body, so a
//!   malformed body cannot panic or error the hot path. A `MalformedLog` for a
//!   missing `log_index` is the only `apply`-time error, which a canonical
//!   finalized log never hits.
//! - **Log ordering / monotonicity.** The store key IS `(contract, block,
//!   log_index)`, so duplicates overwrite in place and the btree returns rows in
//!   canonical order; the engine also sorts each page before `apply`.
//! - **Address / topic confusion.** `apply` resolves `log.address()` to the
//!   [`ContractId`] (address is the authority) and additionally verifies the
//!   `topic0` is declared for that contract; a mismatch is skipped. The
//!   [`EventKey`](store::EventKey) is namespaced by [`ContractId`], so a
//!   misresolution cannot cross-contaminate.
//! - **Decode-time DoS.** A
//!   [`MAX_EVENT_DATA`](store::MAX_EVENT_DATA) cap at `apply` skips an oversized
//!   `data` blob, bounding disk and view-decode allocation.
//! - **Reorg-revert correctness.** `revert(from_block)` is a per-[`ContractId`]
//!   range-delete over the store (and the postage index). Every view derives
//!   purely from raw rows, so deleting the reorged-out range is necessary and
//!   sufficient. The MVP engine indexes finalized-only and never calls it.
//! - **Where validation lives.** The indexer RECORDS; consumers DECIDE. Stamp
//!   signature / owner recovery stays in nectar primitives, never here. The
//!   chain crate announces "advanced"; it never calls a domain trait.
//!
//! # Placement
//!
//! Native, RPC-and-persistence code, intended to run behind a `chain` feature in
//! its consumers and to stay OUT of the wasm cone. alloy is pulled with
//! `default-features = false`; the crate imposes no transport of its own.

mod indexer;
mod registry;
mod store;

pub mod views;

pub use indexer::{ContractIndexer, INDEXER_NAME};
pub use registry::{ContractId, EventDescriptor, Network, WatchedContract, abi, registry};
pub use store::{
    BatchByBalance, BatchKey, BatchState, BatchTable, BatchUpdate, ContractIndexTables, EventKey,
    EventTable, MAX_EVENT_DATA, StoredEvent,
};

#[cfg(test)]
mod tests;
