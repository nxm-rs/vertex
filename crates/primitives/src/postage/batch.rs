use alloy_primitives::Address;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use thiserror::Error;
use tracing::error;

use crate::postage::stamp::{MarshalledStamp, Stamp, StampError, StampValidator, ValidateStamp};
use crate::bmt::chunk::Chunk;

pub type BatchId = [u8; 32];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Batch {
    pub id: BatchId,                // the batch id
    value: u128,                    // normalised balance of the batch
    pub block_created: Option<u64>, // block number the batch was created
    pub(crate) owner: Address,      // owner of the batch
    pub depth: u32,                 // depth of the batch
    pub bucket_depth: u32,          // depth of the bucket
    pub immutable: bool,            // whether the batch is immutable
}

impl Batch {
    pub fn new(
        id: BatchId,
        value: u128,
        start: Option<u64>,
        owner: Address,
        depth: u32,
        bucket_depth: u32,
        immutable: bool,
    ) -> Self {
        Self {
            id,
            value,
            block_created: start,
            owner,
            depth,
            bucket_depth,
            immutable,
        }
    }

    pub fn id(&self) -> BatchId {
        self.id
    }

    pub fn owner(&self) -> Address {
        self.owner
    }

    pub fn depth(&self) -> u32 {
        self.depth
    }

    pub fn bucket_depth(&self) -> u32 {
        self.bucket_depth
    }
}

/// An error involving the batch store
#[derive(Debug, Error)]
pub enum BatchStoreError {
    /// When a batch is not found
    #[error("Batch not found")]
    BatchNotFound(BatchId),
}

pub(crate) struct Store {
    pub batches: Arc<Mutex<HashMap<BatchId, Batch>>>,
}

impl Store {
    pub fn new() -> Self {
        Self {
            batches: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn load(batches: HashMap<BatchId, Batch>) -> Self {
        Self {
            batches: Arc::new(Mutex::new(batches)),
        }
    }

    pub fn get(&self, id: BatchId) -> Option<Batch> {
        self.batches.lock().unwrap().get(&id).cloned()
    }

    pub fn insert(&self, batch: Batch) {
        self.batches.lock().unwrap().insert(batch.id, batch);
    }

    pub fn exists(&self, id: BatchId) -> bool {
        self.batches.lock().unwrap().contains_key(&id)
    }
}

impl StampValidator for Store {
    fn validate_stamp<'a>(&'a self) -> ValidateStamp<'a> {
        let store = self;
        Box::new(move |chunk: &mut Chunk, stamp: MarshalledStamp| {
            let stamp = Stamp::from(stamp);
            match store.get(stamp.batch()) {
                Some(batch) => {
                    match stamp.valid(chunk, batch.owner, batch.depth, batch.bucket_depth) {
                        Ok(_valid) => {
                            chunk.add_stamp(stamp.into());
                            Ok(())
                        }
                        Err(e) => Err(e),
                    }
                }
                None => {
                    error!("Batch not found: {:?}", stamp.batch());
                    Err(StampError::BatchNotFound(stamp.batch()))
                }
            }
        })
    }
}
