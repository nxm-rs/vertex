use alloy::{primitives::B256, signers::Signature};

const STAMP_INDEX_SIZE: usize = std::mem::size_of::<u64>();
const STAMP_TIMESTAMP_SIZE: usize = std::mem::size_of::<u64>();

pub trait Stamp {
    fn batch_id(&self) -> B256;
    fn index(&self) -> u64;
    fn sig(&self) -> Signature;
    fn timestamp(&self) -> u64;
    fn hash(&self) -> B256;
}
