use crate::{ChunkAddress, BRANCHES, SEGMENT_SIZE};

pub const CHUNK_SIZE: usize = SEGMENT_SIZE * BRANCHES;

pub trait Chunk {
    fn address(&self) -> ChunkAddress;
    fn verify(&self, address: ChunkAddress) -> bool;
}

pub trait ChunkBody {
    fn hash(&self) -> ChunkAddress;
}

pub trait ChunkEncoding {
    fn size(&self) -> usize;
    fn to_boxed_slice(&self) -> Box<[u8]>;
}

pub trait ChunkDecoding
where
    Self: Sized,
{
    fn from_slice(buf: &[u8]) -> Result<Self, impl std::error::Error>;
}
