use std::{future::Future, pin::Pin};

use crate::postage::{batch::BatchId, pat::BucketSeeker};
use alloy_primitives::{Address, Keccak256};
use alloy_signer::Signature;
use thiserror::Error;
use tracing::error;

use crate::bmt::chunk::Chunk;

// Define a type alias for a closure that takes a chunk + stamp, and modifies the chunk
// to include the stamp if it is valid
pub type ValidateStamp<'a> =
    Box<dyn FnMut(&mut Chunk, MarshalledStamp) -> Result<(), StampError> + 'a>;

pub type MarshalledStamp = [u8; 113];

pub trait StampValidator {
    fn validate_stamp(&self) -> ValidateStamp<'_>;
}

/// An error involving a stamp
#[derive(Debug, Error)]
pub enum StampError {
    /// Invalid signature
    #[error("owner mismatch, expected {0}, got {1}")]
    OwnerMismatch(Address, Address),
    /// Invalid index
    /// The index is equal to a two `u32` concatenated together where the first `u32` is the
    /// bucket and the second `u32` is the index in the bucket
    #[error("invalid index")]
    InvalidIndex(),
    /// The chunk address does not match the bucket
    #[error("bucket mismatch")]
    BucketMismatch(),
    /// When a batch isn't found in the store
    #[error("batch not found")]
    BatchNotFound(BatchId),
}

/// A `Stamp` represents the proof of postage for a chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stamp {
    batch: BatchId, // the batch id
    x: u32,         // which collision bucket the chunk is in
    y: u32,         // where in bucket the chunk is
    timestamp: u64, // timestamp of the stamp
    sig: [u8; 65],  // signature of the stamp
}

// Define a type alias for a closure that takes a digest and returns a signature
impl Stamp {
    pub async fn new<'a, F>(
        chunk: &Chunk,
        batch: BatchId,
        x: u32,
        y: u32,
        timestamp: u64,
        sig_fn: F,
    ) -> Self
    where
        F: 'a
            + Fn(
                [u8; 32],
            )
                -> Pin<Box<dyn Future<Output = Result<[u8; 65], Box<dyn std::error::Error + 'a>>>>>,
    {
        Self {
            batch,
            x,
            y,
            timestamp,
            sig: (sig_fn)(Self::digest(chunk, batch, x, y, timestamp))
                .await
                .unwrap(),
        }
    }

    /// Returns the hash of the stamp to be signed
    /// This is equal to H(chunkAddr || batchId || sillyIndex || timestamp)
    pub fn digest(chunk: &Chunk, batch: BatchId, x: u32, y: u32, timestamp: u64) -> BatchId {
        let mut hasher = Keccak256::new();
        hasher.update(chunk.address().as_slice());
        hasher.update(&batch);
        hasher.update(&Self::silly_index(x, y).to_be_bytes());
        hasher.update(&timestamp.to_be_bytes());
        *hasher.finalize()
    }

    /// This is equal to a two `u32` concatenated
    /// The first `u32` is the bucket (`x`)
    /// The second `u32` is the bucket index (`y`)
    fn silly_index(x: u32, y: u32) -> u64 {
        let mut bytes = [0u8; 8];
        bytes[..4].copy_from_slice(&x.to_be_bytes());
        bytes[4..].copy_from_slice(&y.to_be_bytes());
        u64::from_be_bytes(bytes)
    }

    pub fn valid(&self, chunk: &Chunk, owner: Address, x: u32, y: u32) -> Result<bool, StampError> {
        // check `x` - we are in the correct bucket
        if self.x != chunk.get_x(y) {
            return Err(StampError::BucketMismatch());
        }

        // check `y` does not exceed the max number of chunks in a bucket
        // this is equal to 2^(depth - bucket_depth) and is zero indexed
        if self.y >= 1 << (x - y) {
            return Err(StampError::InvalidIndex());
        }

        let digest = Self::digest(chunk, self.batch, self.x, self.y, self.timestamp);

        // verify the signature
        // using unwrap() here is safe because we know the signature is 65 bytes
        let signature: Signature = Signature::try_from(self.sig.as_slice()).unwrap();
        signature.recover_address_from_msg(digest.as_slice())
            .map_err(|_| StampError::InvalidIndex())
            .and_then(|recovered| {
                if owner == recovered {
                    Ok(true)
                } else {
                    Err(StampError::OwnerMismatch(owner, recovered))
                }
            })
    }

    pub fn batch(&self) -> [u8; 32] {
        self.batch
    }
}

impl From<Stamp> for MarshalledStamp {
    fn from(stamp: Stamp) -> Self {
        let mut bytes = [0u8; 113];
        bytes[..32].copy_from_slice(&stamp.batch);
        bytes[32..36].copy_from_slice(&stamp.x.to_be_bytes());
        bytes[36..40].copy_from_slice(&stamp.y.to_be_bytes());
        bytes[40..48].copy_from_slice(&stamp.timestamp.to_be_bytes());
        bytes[48..113].copy_from_slice(stamp.sig.to_vec().as_slice());
        bytes
    }
}

impl From<MarshalledStamp> for Stamp {
    fn from(bytes: MarshalledStamp) -> Self {
        let mut batch = [0u8; 32];
        batch.copy_from_slice(&bytes[..32]);
        let x = u32::from_be_bytes(bytes[32..36].try_into().unwrap());
        let y = u32::from_be_bytes(bytes[36..40].try_into().unwrap());
        let timestamp = u64::from_be_bytes(bytes[40..48].try_into().unwrap());
        let mut sig = [0u8; 65];
        sig.copy_from_slice(&bytes[48..113]);
        Self {
            batch,
            x,
            y,
            timestamp,
            sig,
        }
    }
}

impl From<Stamp> for Vec<u8> {
    fn from(stamp: Stamp) -> Self {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&stamp.batch);
        bytes.extend_from_slice(&stamp.x.to_be_bytes());
        bytes.extend_from_slice(&stamp.y.to_be_bytes());
        bytes.extend_from_slice(&stamp.timestamp.to_be_bytes());
        bytes.extend_from_slice(stamp.sig.to_vec().as_slice());
        bytes
    }
}

impl From<Vec<u8>> for Stamp {
    fn from(bytes: Vec<u8>) -> Self {
        let mut batch = [0u8; 32];
        batch.copy_from_slice(&bytes[..32]);
        let x = u32::from_be_bytes(bytes[32..36].try_into().unwrap());
        let y = u32::from_be_bytes(bytes[36..40].try_into().unwrap());
        let timestamp = u64::from_be_bytes(bytes[40..48].try_into().unwrap());
        let mut sig = [0u8; 65];
        sig.copy_from_slice(&bytes[48..113]);
        Self {
            batch,
            x,
            y,
            timestamp,
            sig,
        }
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::hex;

    use super::*;

    struct TestFixture {
        batch_id: Vec<u8>,
        stamp_marshalled: Vec<u8>,
        bucket_index: u32,
        bucket: u32,
        timestamp: u64,
    }

    fn setup() -> TestFixture {
        TestFixture {
            batch_id: hex::decode("c3387832bb1b88acbcd0ffdb65a08ef077d98c08d4bee576a72dbe3d36761369").unwrap(),
            stamp_marshalled: hex::decode("c3387832bb1b88acbcd0ffdb65a08ef077d98c08d4bee576a72dbe3d367613690000cbe5000000000000018921ff0dbb29169df9e6364e26c6ca6b17745c10b9d6a36ea38e204f2e3cc64a8373c0661f5bb0a347c61d8d1689b0dcf8354117686a6a18d08cff927f526de5fc61b2b7491b").unwrap(),
            bucket_index: 0,
            bucket: 52197,
            timestamp: 1688492510651,
        }
    }

    #[test]
    fn stamp_to_bytes() {
        let stamp = Stamp {
            batch: [0u8; 32],
            y: 0,
            x: 0,
            timestamp: 0,
            sig: [0u8; 65],
        };
        let bytes: MarshalledStamp = stamp.into();
        assert_eq!(bytes.len(), 113);
    }

    #[test]
    fn stamp_from_bytes() {
        let fixture = setup();
        let stamp: Stamp = fixture.stamp_marshalled.clone().into();
        assert_eq!(&stamp.batch[..], &fixture.batch_id[..]);
        assert_eq!(stamp.x, fixture.bucket);
        assert_eq!(stamp.y, fixture.bucket_index);
        assert_eq!(stamp.timestamp, fixture.timestamp);
        assert_eq!(stamp.sig, fixture.stamp_marshalled[48..113]);
    }

    #[test]
    fn stamp_to_vec() {
        let fixture = setup();
        let stamp: Stamp = fixture.stamp_marshalled.clone().into();
        let vec: Vec<u8> = stamp.clone().into();
        let bytes: MarshalledStamp = stamp.into();

        assert_eq!(vec, bytes.to_vec());
    }

    #[test]
    fn stamp_from_vec() {
        let fixture = setup();
        let stamp: Stamp = fixture.stamp_marshalled.clone().into();
        let vec: Vec<u8> = stamp.clone().into();
        let stamp_from_vec: Stamp = vec.into();

        assert_eq!(stamp, stamp_from_vec);
    }
}