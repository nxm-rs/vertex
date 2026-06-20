//! Postage batch storage and the on-chain event ingest seam for the vertex
//! Swarm node.
//!
//! This crate is the node-side home for postage state. It does not redefine any
//! postage primitive: the batch model, the stamp model, validation and the
//! event vocabulary all live in [`nectar_postage`] and are re-exported verbatim
//! from this crate's root so downstream crates depend on one canonical copy of
//! each type. What this crate adds is the *node's* concrete persistence and the
//! *seam* through which on-chain batch state is driven into that persistence:
//!
//! - [`DbBatchStore`]: a [`nectar_postage::BatchStore`] backed by the
//!   `vertex-storage` `Database` (redb in production). It persists batches and
//!   the [`PostageContext`] across restarts, so a node recovers its full batch
//!   set without replaying the whole chain.
//! - [`DbBatchStore`] also implements [`nectar_postage::BatchEventHandler`]:
//!   this is the ingest seam. The four [`BatchEvent`] variants map onto the
//!   store mutations that keep the node's batch set in step with the contract.
//!
//! # Stamp validation is stateless (no per-batch public-key cache)
//!
//! A stamp is valid iff `ecrecover(sig, digest)` resolves to the batch owner
//! and the index/bucket bounds hold. The node already knows `batch.owner` from
//! the batch store, so validation needs no side state: recover the signer per
//! stamp and compare to the owner (this is exactly what
//! [`nectar_postage::StoreValidator`] does, reusing [`Stamp::recover_signer`]
//! and the nectar digest). This crate therefore ships **no** per-batch
//! public-key cache.
//!
//! nectar does support a cheaper path - recover the signer once with a full
//! ecrecover ([`Stamp::recover_pubkey`]) and verify the remaining stamps of the
//! same batch against that key ([`Stamp::verify_with_pubkey`]). That is a
//! deliberate *future* optimisation: if profiling justifies it, it should be a
//! per-process in-memory memoisation gated behind the batch-existence check,
//! and **never** a persisted sidecar table. It is intentionally not implemented
//! here, so that removing the cache cannot change validation semantics.
//!
//! # The ingest seam (and what is deliberately out of scope)
//!
//! Postage state on Swarm is owned by the postage-stamp contract on the
//! settlement chain. A node learns about new batches, top-ups, dilutions and
//! expiries by watching that contract's logs. The decode path is:
//!
//! ```text
//!   chain logs --> [ PostageIndexer ] --> BatchEvent --> [ BatchEventHandler ]
//!   (raw ABI)      (external crate)       (nectar)        (DbBatchStore here)
//! ```
//!
//! This crate owns only the right-hand half: given a [`BatchEvent`], it applies
//! the correct store mutation atomically (see [`DbBatchStore::handle_event`]).
//!
//! The left-hand half - the event ABI, the log decoder, and the indexer that
//! pulls logs from a chain provider and reduces them into [`BatchEvent`]s - is
//! **intentionally stubbed / out of scope** in this crate. That work is owned
//! externally (a `PostageIndexer`) and is wired up by the node builder, not
//! here. Concretely, this crate defines and documents the *seam*
//! ([`BatchEventHandler`], re-exported, implemented by [`DbBatchStore`]) but
//! ships no contract bindings, no `alloy_sol_types` event definitions, and no
//! log-to-[`BatchEvent`] reducer. Keeping the two halves apart lets the chain
//! indexer evolve (ABI revisions, reorg handling, batched log fetches) without
//! touching the persistence layer, and lets this crate be tested with synthetic
//! [`BatchEvent`]s and no chain at all.
//!
//! # Consensus note: keep stamp identity, not just the batch
//!
//! The store keys batches by [`BatchId`] alone, which is correct: a batch is a
//! single on-chain object. It is the *reserve* and the *sampler* (other crates)
//! that must key stamped entries by the full `(batchID, 8-byte stampIndex)` and
//! carry the precise winning stamp through an inclusion proof. This crate's job
//! stops at making the batch available for stamp validation; it does not
//! collapse distinct stamps of a batch.

#![cfg_attr(docsrs, feature(doc_cfg))]

mod admission;
mod stampindex;
mod store;

// -------------------------------------------------------------------------
// Re-export the nectar postage public surface.
//
// Downstream vertex crates depend on `vertex-swarm-postage` for postage types
// and get exactly the nectar definitions, so there is a single canonical copy
// of `Batch`, `Stamp`, `BatchEvent`, the `BatchStore`/`BatchEventHandler`
// traits and the validators across the workspace.
// -------------------------------------------------------------------------
pub use nectar_postage::{
    Batch, BatchEvent, BatchEventHandler, BatchId, BatchParams, BatchStore, BatchStoreError,
    BatchStoreExt, PostageContext, STAMP_SIZE, Stamp, StampBytes, StampDigest, StampError,
    StampIndex, StampValidator, StoreValidator, VerifyingKey, calculate_bucket, current_timestamp,
};

// This crate's own additions.
pub use admission::{AdmissionError, AdmissionValidator};
pub use stampindex::{
    Arbitration, DbStampIndexArbiter, DisplacedEntry, IncomingStamp, RejectReason,
    StampIndexArbiter, StampIndexEntry, StampIndexError, StampIndexTable, StampSlotKey, decide,
};
pub use store::{DbBatchStore, DbBatchStoreError};
