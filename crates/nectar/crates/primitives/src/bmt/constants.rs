//! Constants used in the Binary Merkle Tree implementation.

/// Default hash size in bytes
pub const HASH_SIZE: usize = 32;

/// Size of a segment in the BMT (same as hash size)
pub const SEGMENT_SIZE: usize = HASH_SIZE;

/// Number of branches in the Binary Merkle Tree
pub const BMT_BRANCHES: usize = 128;

/// The depth of the Binary Merkle Tree based on BMT_BRANCHES
pub const BMT_DEPTH: usize = 8; // log2(128) + 1 = 8

/// The max data length for the Binary Merkle Tree (number of segments * segment size)
pub const BMT_MAX_DATA_LENGTH: usize = BMT_BRANCHES * SEGMENT_SIZE;

/// The length of a segment pair (two segments)
pub const SEGMENT_PAIR_LENGTH: usize = 2 * SEGMENT_SIZE;
