//! Node-side postage state: batch persistence and the on-chain event ingest
//! seam.
//!
//! Postage primitives (batch and stamp models, validation, the event
//! vocabulary) live in [`nectar_postage`] and are re-exported verbatim so the
//! workspace shares one canonical copy of each type. This crate adds:
//!
//! - [`DbBatchStore`]: a [`nectar_postage::BatchStore`] backed by the
//!   `vertex-storage` `Database`, persisting batches and the [`PostageContext`]
//!   so a node recovers its batch set without replaying the chain.
//! - [`DbBatchStore`]'s [`nectar_postage::BatchEventHandler`] impl: the ingest
//!   seam mapping each [`BatchEvent`] onto an atomic store mutation (see
//!   [`DbBatchStore::handle_event`]).
//!
//! Stamp validation is stateless: a stamp is valid iff `ecrecover(sig, digest)`
//! resolves to the batch owner and the index/bucket bounds hold. The owner is
//! already in the batch store, so [`nectar_postage::StoreValidator`] needs no
//! side state and this crate ships no per-batch public-key cache.
//!
//! Ingest pipeline:
//!
//! ```text
//!   chain logs --> [ PostageIndexer ] --> BatchEvent --> [ BatchEventHandler ]
//!   (raw ABI)      (external crate)       (nectar)        (DbBatchStore here)
//! ```
//!
//! Only the right-hand half lives here. The event ABI, log decoder, and the
//! indexer that reduces logs into [`BatchEvent`]s are out of scope, owned by an
//! external `PostageIndexer` wired up by the node builder.
//!
//! The store keys batches by [`BatchId`] alone (a batch is one on-chain object).
//! Keying stamped entries by the full `(batchID, 8-byte stampIndex)` and
//! carrying the winning stamp through an inclusion proof is the reserve's and
//! sampler's job, not this crate's.

#![cfg_attr(docsrs, feature(doc_cfg))]

mod admission;
mod stampindex;
mod store;

pub use nectar_postage::{
    Batch, BatchEvent, BatchEventHandler, BatchId, BatchParams, BatchStore, BatchStoreError,
    BatchStoreExt, PostageContext, STAMP_SIZE, Stamp, StampBytes, StampDigest, StampError,
    StampIndex, StampValidator, StoreValidator, VerifyingKey, calculate_bucket, current_timestamp,
};

pub use admission::{AdmissionError, AdmissionValidator};
pub use stampindex::{
    Arbitration, DbStampIndexArbiter, DisplacedEntry, IncomingStamp, RejectReason,
    StampIndexArbiter, StampIndexEntry, StampIndexError, StampIndexTable, StampSlotKey, decide,
};
pub use store::{DbBatchStore, DbBatchStoreError};
