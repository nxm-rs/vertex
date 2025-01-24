use swarm_primitives_traits::{ChunkAddress, ChunkBody, ChunkDecoding, ChunkEncoding};

use super::bmt_body::{BMTBody, BMTBodyError};

#[derive(Debug, thiserror::Error)]
pub enum ContentChunkError {
    #[error("BMTBody error: {0}")]
    BMTBodyError(#[from] BMTBodyError),
}

#[derive(Debug, PartialEq, Eq)]
pub struct ContentChunk {
    body: BMTBody,
}

impl ContentChunk {
    pub fn new(data: Vec<u8>) -> Result<Self, ContentChunkError> {
        Ok(Self {
            body: BMTBody::new(data.len() as u64, data)?,
        })
    }

    pub fn new_with_span(span: u64, data: Vec<u8>) -> Result<Self, ContentChunkError> {
        Ok(Self {
            body: BMTBody::new(span, data)?,
        })
    }
}

impl swarm_primitives_traits::Chunk for ContentChunk {
    fn address(&self) -> swarm_primitives_traits::ChunkAddress {
        self.body.hash()
    }

    fn verify(&self, address: ChunkAddress) -> bool {
        address == self.address()
    }
}

impl ChunkEncoding for ContentChunk {
    fn size(&self) -> usize {
        self.body.size()
    }

    fn to_boxed_slice(&self) -> Box<[u8]> {
        self.body.to_boxed_slice()
    }
}

impl ChunkDecoding for ContentChunk {
    #[allow(refining_impl_trait)]
    fn from_slice(buf: &[u8]) -> Result<Self, ContentChunkError> {
        Ok(Self {
            body: BMTBody::from_slice(buf)?,
        })
    }
}
