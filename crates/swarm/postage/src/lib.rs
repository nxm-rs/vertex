//! Node-side postage state for the vertex Swarm node.
//!
//! Postage primitives (batch and stamp models, validation, events) live in
//! [`nectar_postage`] and are re-exported. This crate adds [`DbBatchStore`], a
//! [`nectar_postage::BatchStore`] over the `vertex-storage` `Database`, and its
//! [`nectar_postage::BatchEventHandler`] impl. With the `chain` feature it also
//! ships [`index`](crate::index): the on-chain PostageStamp tag, ABIs, reducer,
//! and views that fold contract logs into the same `Batches` table the store
//! persists.

#![cfg_attr(docsrs, feature(doc_cfg))]

mod admission;
#[cfg(feature = "chain")]
pub mod index;
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
