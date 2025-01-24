use alloy_primitives::{PrimitiveSignature, B256};

const STAMP_INDEX_SIZE: usize = std::mem::size_of::<u64>();
const STAMP_TIMESTAMP_SIZE: usize = std::mem::size_of::<u64>();

pub trait Stamp {
    fn batch_id(&self) -> B256;
    fn index(&self) -> u64;
    fn sig(&self) -> PrimitiveSignature;
    fn timestamp(&self) -> u64;
    fn hash(&self) -> B256;
}
