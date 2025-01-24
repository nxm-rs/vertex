mod content;
use content::ContentChunk;
mod single_owner;
use single_owner::SingleOwnerChunk;
mod bmt_body;
use swarm_primitives_traits::{Chunk as ChunkTrait, ChunkAddress};

#[derive(Debug, Eq, PartialEq)]
pub enum Chunk {
    Content(ContentChunk),
    SingleOwner(SingleOwnerChunk),
}

impl Chunk {
    pub fn verify(&self, address: ChunkAddress) -> bool {
        match self {
            Chunk::Content(c) => c.verify(address),
            Chunk::SingleOwner(c) => c.verify(address),
        }
    }
}
