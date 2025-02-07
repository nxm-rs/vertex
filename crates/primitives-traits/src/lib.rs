use alloy::primitives::FixedBytes;

pub mod auth;
pub mod chunk;

pub use auth::*;
pub use chunk::*;

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
