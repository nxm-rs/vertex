use alloy_primitives::FixedBytes;

pub mod chunk;
pub mod stamp;

pub use chunk::{Chunk, ChunkBody, ChunkDecoding, ChunkEncoding, CHUNK_SIZE};
pub use stamp::Stamp;

const HASH_SIZE: usize = 32;
pub const SEGMENT_SIZE: usize = HASH_SIZE;
pub const BRANCHES: usize = 128;

// Addresses
pub type SwarmAddress = FixedBytes<SEGMENT_SIZE>;
pub type NodeAddress = SwarmAddress;

// BMT / Chunks
pub type ChunkAddress = SwarmAddress;
pub type Segment = [u8; SEGMENT_SIZE];
pub type Span = u64;
pub const SPAN_SIZE: usize = std::mem::size_of::<Span>();
