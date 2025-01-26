use crate::{ChunkAddress, BRANCHES, SEGMENT_SIZE};

pub const CHUNK_SIZE: usize = SEGMENT_SIZE * BRANCHES;

pub trait Chunk {
    fn address(&self) -> ChunkAddress;

    fn verify(&self, address: ChunkAddress) -> bool {
        self.address() == address
    }
}

pub trait ChunkBody {
    fn hash(&self) -> ChunkAddress;
    fn data(&self) -> &[u8];
}
