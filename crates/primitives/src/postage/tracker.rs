use bytes::Bytes;
use nectar_primitives_traits::SwarmAddress;
use serde::{Deserialize, Serialize};

use super::Batch;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostageStampTracker {
    label: String,
    batch: Batch,
    buckets: Vec<u32>,
    max_bucket_count: u32,
    counter: u32,
}

impl PostageStampTracker {
    pub fn increment(&self, chunk_addr: SwarmAddress) -> (u64, u64) {
        let bucket_index = to_bucket_index(self.batch.bucket_depth(), chunk_addr);
        let mut bucket_count = self.buckets[bucket_index as usize];

        if bucket_count as u64 < self.batch.max_collisions() {
            if self.batch.immutable() {
                panic!("bucket full");
            }

            bucket_count = 0;
            self.buckets[bucket_index as usize] = bucket_count;
        }

        self.buckets[bucket_index as usize] += 1;
        if self.buckets[bucket_index as usize] > self.max_bucket_count {
            self.max_bucket_count = self.buckets[bucket_index as usize];
        }

        (
            pack_index_to_u64(bucket_index, bucket_count),
            self.counter as u64,
        )
    }
}

/// Returns the index of the collision bucket for a given chunk address.
///
/// # Arguments
/// * `bucket_depth` - The collision bucket depth, which determines the number of bits to consider.
///             Must be less than or equal to 32; otherwise, the function will panic.
/// * `chunk_addr` - The chunk address whose collision bucket index is to be calculated.
///
/// # Panics
/// Panics if `bucket_depth` is greater than 32.
fn to_bucket_index(bucket_depth: u8, chunk_addr: SwarmAddress) -> u32 {
    if bucket_depth > 32 {
        panic!("Depth must be less than or equal to 32.");
    }

    let prefix_u32 = u32::from_be_bytes(
        Bytes::from(chunk_addr)
            .split_at(4)
            .0
            .try_into()
            .expect("SwarmAddress is guaranteed to be 32 bytes long"),
    );

    prefix_u32 >> (32 - bucket_depth)
}

/// Packs a bucket index and a slot index into a single u64 value.
///
/// # Arguments
/// * `bucket` - The collision bucket index of the batch for the chunk.
/// * `slot` - The slot index of the batch for the chunk.
fn pack_index_to_u64(bucket: u32, slot: u32) -> u64 {
    ((bucket as u64) << 32) | slot as u64
}

/// Packs a bucket index and a slot index into a bytes value.
///
/// # Arguments
/// * `bucket` - The collision bucket index of the batch for the chunk.
/// * `slot` - The slot index of the batch for the chunk.
fn pack_index_to_bytes(bucket: u32, slot: u32) -> Bytes {
    pack_index_to_u64(bucket, slot).to_be_bytes().into()
}

/// Unpacks a u64 value into a bucket index and a slot index.
/// Returns a tuple of the bucket index and the slot index.
///
/// # Arguments
/// * `index` - The u64 value to be unpacked.
fn unpack_index_from_u64(index: u64) -> (u32, u32) {
    let bucket = (index >> 32) as u32;
    let slot = index as u32;
    (bucket, slot)
}

/// Unpacks a bytes value into a bucket index and a slot index.
/// Returns a tuple of the bucket index and the slot index.
///
/// # Arguments
/// * `index` - The bytes value to be unpacked.
///
/// # Panics
/// Panics if the bytes value is not 8 bytes long.
fn unpack_index_from_bytes(mut index: Bytes) -> (u32, u32) {
    let bucket: u32 = u32::from_be_bytes(
        index
            .split_to(4)
            .as_ref()
            .try_into()
            .expect("index is 8 bytes long"),
    );
    let slot: u32 = u32::from_be_bytes(index.as_ref().try_into().expect("index is 8 bytes long"));

    (bucket, slot)
}
