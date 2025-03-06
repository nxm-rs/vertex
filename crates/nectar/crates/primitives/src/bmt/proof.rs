//! Proof related traits and structures for the Binary Merkle Tree.

use alloy_primitives::Keccak256;
use auto_impl::auto_impl;

use super::hasher::BMTHasher;
use crate::constants::*;
use crate::error::{DigestError, Result};

/// Depth of the Binary Merkle Tree for proofs
const BMT_PROOF_LENGTH: usize = BMT_DEPTH - 1;

/// Represents a proof for a specific segment in a Binary Merkle Tree
#[derive(Clone, Debug)]
pub struct BMTProof {
    /// The segment index this proof is for
    pub segment_index: usize,
    /// The segment data being proven
    pub segment: [u8; SEGMENT_SIZE],
    /// The proof segments (sibling hashes in the path to the root)
    pub proof_segments: Vec<[u8; SEGMENT_SIZE]>,
    /// The span of the data
    pub span: u64,
}

impl BMTProof {
    /// Create a new BMT proof
    pub fn new(
        segment_index: usize,
        segment: [u8; SEGMENT_SIZE],
        proof_segments: Vec<[u8; SEGMENT_SIZE]>,
        span: u64,
    ) -> Self {
        Self {
            segment_index,
            segment,
            proof_segments,
            span,
        }
    }

    /// Generate a proof for a specific segment within BMT data
    pub fn generate(data: &[u8], segment_index: usize, span: u64) -> Result<Self> {
        if segment_index >= BMT_BRANCHES {
            return Err(DigestError::ComputationFailed(format!(
                "Segment index {segment_index} out of bounds for BMT_BRANCHES"
            ))
            .into());
        }

        // Split data into segments
        let mut segments = Vec::with_capacity(BMT_BRANCHES);
        let data_len = data.len();

        for i in 0..BMT_BRANCHES {
            let start = i * SEGMENT_SIZE;
            let mut segment = [0u8; SEGMENT_SIZE];

            if start < data_len {
                let end = (start + SEGMENT_SIZE).min(data_len);
                let copy_len = end - start;
                segment[..copy_len].copy_from_slice(&data[start..end]);
            }

            segments.push(segment);
        }

        // Get the segment being proven
        let segment = segments[segment_index];

        // Generate proof segments
        let mut proof_segments = Vec::with_capacity(BMT_PROOF_LENGTH);

        // Track current position in tree
        let mut current_index = segment_index;

        // Add the sibling at the leaf level
        let sibling_index = if current_index % 2 == 0 {
            current_index + 1
        } else {
            current_index - 1
        };
        if sibling_index < segments.len() {
            proof_segments.push(segments[sibling_index]);
        } else {
            proof_segments.push([0u8; SEGMENT_SIZE]);
        }

        // Move up the tree
        current_index /= 2;

        // Compute the nodes at each level and collect siblings
        let mut level_nodes = compute_level_nodes(&segments);
        let mut level_size = level_nodes.len();

        while proof_segments.len() < BMT_PROOF_LENGTH {
            // Get the sibling at this level
            let sibling_index = if current_index % 2 == 0 {
                current_index + 1
            } else {
                current_index - 1
            };

            if sibling_index < level_size {
                proof_segments.push(level_nodes[sibling_index]);
            } else {
                proof_segments.push([0u8; SEGMENT_SIZE]);
            }

            // Move to next level up
            current_index /= 2;

            // Compute the next level nodes
            level_nodes = compute_level_nodes(&level_nodes);
            level_size = level_nodes.len();

            if level_size <= 1 {
                // We've reached the root, add remaining zero hashes if needed
                while proof_segments.len() < BMT_PROOF_LENGTH {
                    proof_segments.push([0u8; SEGMENT_SIZE]);
                }
                break;
            }
        }

        Ok(Self::new(segment_index, segment, proof_segments, span))
    }

    /// Verify this proof against a root hash
    pub fn verify(&self, root_hash: &[u8]) -> Result<bool> {
        if self.proof_segments.len() != BMT_PROOF_LENGTH {
            return Err(DigestError::VerificationFailed("Invalid proof length".into()).into());
        }

        // Start with the segment being proven
        let mut current_hash = self.segment;
        let mut current_index = self.segment_index;

        // Apply each proof segment to compute the root
        for proof_segment in &self.proof_segments {
            let mut hasher = Keccak256::new();

            // Order matters - left then right
            if current_index % 2 == 0 {
                hasher.update(current_hash);
                hasher.update(proof_segment);
            } else {
                hasher.update(proof_segment);
                hasher.update(current_hash);
            }

            // Get hash for next level
            current_hash.copy_from_slice(hasher.finalize().as_slice());
            current_index /= 2;
        }

        // Final step: add span to compute the root hash
        let mut hasher = Keccak256::new();
        hasher.update(self.span.to_le_bytes());
        hasher.update(current_hash);

        let mut computed_root = [0u8; HASH_SIZE];
        computed_root.copy_from_slice(hasher.finalize().as_slice());

        // Compare with provided root hash
        Ok(computed_root.as_slice() == root_hash)
    }
}

/// Compute the nodes at the next level up in the tree
fn compute_level_nodes(nodes: &[[u8; SEGMENT_SIZE]]) -> Vec<[u8; SEGMENT_SIZE]> {
    let mut result = Vec::with_capacity((nodes.len() + 1) / 2);

    for chunk in nodes.chunks(2) {
        let mut hasher = Keccak256::new();

        // Add left node
        hasher.update(chunk[0]);

        // Add right node if it exists, otherwise use zeros
        if chunk.len() > 1 {
            hasher.update(chunk[1]);
        } else {
            hasher.update([0u8; SEGMENT_SIZE]);
        }

        // Create the parent node
        let mut node = [0u8; SEGMENT_SIZE];
        node.copy_from_slice(hasher.finalize().as_slice());
        result.push(node);
    }

    result
}

/// Extension trait to add proof-related functionality to BMTHasher
pub trait BmtProver {
    /// Generate a proof for a specific segment
    fn generate_proof(&self, data: &[u8], segment_index: usize) -> Result<BMTProof>;

    /// Verify a proof against a root hash
    fn verify_proof(proof: &BMTProof, root_hash: &[u8]) -> Result<bool>;
}

impl BmtProver for BMTHasher {
    fn generate_proof(&self, data: &[u8], segment_index: usize) -> Result<BMTProof> {
        BMTProof::generate(data, segment_index, self.span())
    }

    fn verify_proof(proof: &BMTProof, root_hash: &[u8]) -> Result<bool> {
        proof.verify(root_hash)
    }
}

/// Factory trait for creating digest instances
#[auto_impl(&, Arc)]
pub trait DigestFactory: Send + Sync + 'static {
    /// Create a new BMT hasher instance
    fn create_bmt_hasher(&self) -> Box<BMTHasher>;
}
