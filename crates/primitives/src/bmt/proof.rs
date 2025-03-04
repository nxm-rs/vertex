use crate::bmt::{Hasher, Segment, Span, TreeIterator, DEPTH};
use alloy_primitives::Keccak256;
use nectar_primitives_traits::{BRANCHES, SEGMENT_SIZE};
use thiserror::Error;

const PROOF_LENGTH: usize = DEPTH - 1;

/// The `Prover` trait provides functionality for creating and verifying Merkle proofs over a
/// binary Merkle tree (BMT). It defines methods to:
///
/// 1. Generate inclusion proofs for specific segments within the tree.
/// 2. Verify these proofs against the tree's root hash.
///
/// The trait is implemented by the [`Hasher`] struct, which manages hashing operations and
/// interacts with the underlying tree structure.
pub trait Prover {
    /// Generates an inclusion proof for the `i`-th data segment.
    ///
    /// # Arguments
    ///
    /// * `i` - The index of the segment for which the proof is generated.
    ///
    /// # Returns
    ///
    /// A [`Result`] containing:
    /// - A [`Proof`] struct if the operation is successful.
    /// - A [`ProverError`] varient if an error occurs.
    fn proof(&self, i: usize) -> Result<Proof, ProverError>;
    /// Verifies an inclusion proof and derives the root hash of the tree.
    ///
    /// # Arguments
    ///
    /// * `i` - The index of the segment being verified.
    /// * `proof` - The [`Proof`] struct containing the inclusion proof data.
    ///
    /// # Returns
    ///
    /// A [`Result`] containing:
    /// - The calculated root hash if verification succeeds.
    /// - A [`ProverError`] variant if verification fails.
    fn verify(i: usize, proof: Proof) -> Result<Segment, ProverError>;
}

/// Represents a Merkle proof for a specific segment within a binary Merkle tree.
///
/// A Merkle proof demonstrates the inclusion of a segment in a binary Merkle tree, using sibling
/// hashes to reconstruct the root hash.
#[derive(Debug, Clone)]
pub struct Proof {
    /// The segment being prove for inclusion in the tree.
    pub prove_segment: Segment,
    /// An array of sibling hashes needed to reconstruct the root hash.
    pub proof_segments: [[u8; SEGMENT_SIZE]; PROOF_LENGTH],
    /// The span of the data being hashed.
    pub span: Span,
    /// The index of the segment within the tree
    pub index: usize,
}

/// Represents errors that can occur during Merkle proof generation and verification.
#[derive(Debug, Error)]
pub enum ProverError {
    #[error("Index {0} out of bounds for BMT_BRANCHES")]
    IndexOutOfBounds(usize),
    #[error("Tree iterator unexpectedly empty")]
    IteratorEmpty,
    #[error("Expected level 1, but got level {0}")]
    UnexpectedLevel(usize),
    #[error("Failed to collect proof segments: array mismatch")]
    ProofCollectionFailed,
}

impl Prover for Hasher {
    fn proof(&self, i: usize) -> Result<Proof, ProverError> {
        if i >= BRANCHES {
            return Err(ProverError::IndexOutOfBounds(i));
        }

        let mut tree_iterator = TreeIterator::new(self.tree.clone(), i / 2);

        // Handle the special case for level 1: First element in the iterator
        let (_, (left, right), _, level, index) =
            tree_iterator.next().ok_or(ProverError::IteratorEmpty)?;

        if level != 1 {
            return Err(ProverError::UnexpectedLevel(level));
        }

        let (prove_segment, first_proof_segment): (&Segment, &Segment) = if i % 2 == 0 {
            (left, right)
        } else {
            (right, left)
        };

        let mut proof_segments: [Segment; PROOF_LENGTH] = [[0u8; SEGMENT_SIZE]; PROOF_LENGTH];
        proof_segments[0] = *first_proof_segment;

        let mut proof_index = 1;
        let mut from_left = index % 2 == 0;
        for (_, (left, right), _, _, index) in tree_iterator {
            proof_segments[proof_index] = if from_left { *right } else { *left };

            proof_index += 1;
            from_left = index % 2 == 0;
        }

        Ok(Proof {
            prove_segment: *prove_segment,
            proof_segments,
            span: self.span,
            index: i,
        })
    }

    fn verify(mut i: usize, proof: Proof) -> Result<Segment, ProverError> {
        if proof.proof_segments.len() != PROOF_LENGTH {
            return Err(ProverError::ProofCollectionFailed);
        }

        // Initialise hasher and compute the initial hash
        let mut hasher = Keccak256::new();
        if i % 2 == 0 {
            hasher.update(proof.prove_segment);
            hasher.update(proof.proof_segments[0]);
        } else {
            hasher.update(proof.proof_segments[0]);
            hasher.update(proof.prove_segment);
        }

        let mut current_hash: Segment = [0u8; SEGMENT_SIZE];
        hasher.finalize_into(&mut current_hash);

        i /= 2;

        // Iterate over the remaining proof segments
        for proof_segment in proof.proof_segments.iter().skip(1) {
            let mut hasher = Keccak256::new();
            if i % 2 == 0 {
                hasher.update(current_hash.as_slice());
                hasher.update(proof_segment);
            } else {
                hasher.update(proof_segment);
                hasher.update(current_hash.as_slice());
            }

            hasher.finalize_into(&mut current_hash);

            i /= 2;
        }

        // Combine the final hash with the span to compute the root hash
        let mut hasher = Keccak256::new();
        hasher.update(&proof.span.to_le_bytes());
        hasher.update(&current_hash);

        let mut root_hash: Segment = [0u8; SEGMENT_SIZE];
        hasher.finalize_into(&mut root_hash);

        Ok(root_hash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bmt::{Pool, PooledHasher};
    use alloy_primitives::hex;
    use nectar_primitives_traits::CHUNK_SIZE;
    use rand::Rng;
    use std::sync::Arc;

    #[tokio::test]
    async fn test_proof_correctness() {
        let mut buf = vec![0u8; CHUNK_SIZE];
        let data = b"hello world";
        buf[..data.len()].copy_from_slice(data);

        let pool = Arc::new(Pool::new(1).await);
        let mut prover = pool.get_hasher().await.unwrap();
        let _ = prover.set_span(buf.len() as u64);
        prover.write(&buf).unwrap();

        let mut output = [0u8; SEGMENT_SIZE];
        prover.hash(&mut output);

        let verify_segments = |expected: &[&str], found: &[Segment]| {
            assert_eq!(
                expected.len(),
                found.len(),
                "Incorrect number of proof segments"
            );

            for (expected, actual) in expected.iter().zip(found.iter()) {
                let decoded: Segment = hex::decode(expected)
                    .expect("Invalid hex encoding")
                    .try_into()
                    .expect("Slice size mismatch");
                assert_eq!(&decoded, actual, "Incorrect segment in proof");
            }
        };

        // Test leftmost segment
        {
            let proof = prover.proof(0).unwrap();
            let expected_segments = [
                "0000000000000000000000000000000000000000000000000000000000000000",
                "ad3228b676f7d3cd4284a5443f17f1962b36e491b30a40b2405849e597ba5fb5",
                "b4c11951957c6f8f642c4af61cd6b24640fec6dc7fc607ee8206a99e92410d30",
                "21ddb9a356815c3fac1026b6dec5df3124afbadb485c9ba5a3e3398a04b7ba85",
                "e58769b32a1beaf1ea27375a44095a0d1fb664ce2dd358e7fcbfb78c26a19344",
                "0eb01ebfc9ed27500cd4dfc979272d1f0913cc9f66540d7e8005811109e1cf2d",
                "887c22bd8750d34016ac3c66b5ff102dacdd73f6b014e710b51e8022af9a1968",
            ];

            println!("proof: {:?}", proof);

            verify_segments(&expected_segments, &proof.proof_segments);
            assert_eq!(
                &buf[..SEGMENT_SIZE],
                &proof.prove_segment,
                "Incorrect leftmost segment"
            );
            assert_eq!(
                proof.span, CHUNK_SIZE as u64,
                "Incorrect span for leftmost proof"
            );
        }

        // Test rightmost segment
        {
            let proof = prover.proof(127).unwrap();
            let expected_segments = [
                "0000000000000000000000000000000000000000000000000000000000000000",
                "ad3228b676f7d3cd4284a5443f17f1962b36e491b30a40b2405849e597ba5fb5",
                "b4c11951957c6f8f642c4af61cd6b24640fec6dc7fc607ee8206a99e92410d30",
                "21ddb9a356815c3fac1026b6dec5df3124afbadb485c9ba5a3e3398a04b7ba85",
                "e58769b32a1beaf1ea27375a44095a0d1fb664ce2dd358e7fcbfb78c26a19344",
                "0eb01ebfc9ed27500cd4dfc979272d1f0913cc9f66540d7e8005811109e1cf2d",
                "745bae095b6ff5416b4a351a167f731db6d6f5924f30cd88d48e74261795d27b",
            ];

            verify_segments(&expected_segments, &proof.proof_segments);
            assert_eq!(
                &buf[127 * SEGMENT_SIZE..],
                &proof.prove_segment,
                "Incorrect rightmost segment"
            );
            assert_eq!(
                proof.span, CHUNK_SIZE as u64,
                "Incorrect span for rightmost proof"
            );
        }

        // Test middle segment
        {
            let proof = prover.proof(64).unwrap();
            let expected_segments = [
                "0000000000000000000000000000000000000000000000000000000000000000",
                "ad3228b676f7d3cd4284a5443f17f1962b36e491b30a40b2405849e597ba5fb5",
                "b4c11951957c6f8f642c4af61cd6b24640fec6dc7fc607ee8206a99e92410d30",
                "21ddb9a356815c3fac1026b6dec5df3124afbadb485c9ba5a3e3398a04b7ba85",
                "e58769b32a1beaf1ea27375a44095a0d1fb664ce2dd358e7fcbfb78c26a19344",
                "0eb01ebfc9ed27500cd4dfc979272d1f0913cc9f66540d7e8005811109e1cf2d",
                "745bae095b6ff5416b4a351a167f731db6d6f5924f30cd88d48e74261795d27b",
            ];

            verify_segments(&expected_segments, &proof.proof_segments);
            assert_eq!(
                &buf[64 * SEGMENT_SIZE..65 * SEGMENT_SIZE],
                &proof.prove_segment,
                "Incorrect middle segment"
            );
            assert_eq!(
                proof.span, CHUNK_SIZE as u64,
                "Incorrect span for middle proof"
            );
        }
    }

    #[tokio::test]
    async fn test_root_hash_calculation() {
        let segment_strings = [
            "0000000000000000000000000000000000000000000000000000000000000000",
            "ad3228b676f7d3cd4284a5443f17f1962b36e491b30a40b2405849e597ba5fb5",
            "b4c11951957c6f8f642c4af61cd6b24640fec6dc7fc607ee8206a99e92410d30",
            "21ddb9a356815c3fac1026b6dec5df3124afbadb485c9ba5a3e3398a04b7ba85",
            "e58769b32a1beaf1ea27375a44095a0d1fb664ce2dd358e7fcbfb78c26a19344",
            "0eb01ebfc9ed27500cd4dfc979272d1f0913cc9f66540d7e8005811109e1cf2d",
            "745bae095b6ff5416b4a351a167f731db6d6f5924f30cd88d48e74261795d27b",
        ];

        let mut segments = [[0u8; SEGMENT_SIZE]; 7];
        for (i, s) in segment_strings.iter().enumerate() {
            segments[i] = hex::decode(s)
                .expect("Invalid hex encoding")
                .try_into()
                .expect("Slice size mismatch");
        }

        let mut buf = vec![0u8; CHUNK_SIZE];
        let data = b"hello world";
        buf[..data.len()].copy_from_slice(data);

        let pool = Arc::new(Pool::new(1).await);
        let mut prover = pool.get_hasher().await.unwrap();
        let _ = prover.set_span(buf.len() as u64);
        prover.write(&buf).unwrap();

        let proof = Proof {
            prove_segment: buf[64 * SEGMENT_SIZE..65 * SEGMENT_SIZE]
                .try_into()
                .expect("Slice size mismatch"),
            proof_segments: segments,
            span: 4096,
            index: 64,
        };

        let root_hash = Hasher::verify(64, proof).unwrap();
        let mut expected_root_hash = [0u8; SEGMENT_SIZE];
        prover.hash(&mut expected_root_hash);

        assert_eq!(
            root_hash, expected_root_hash,
            "Incorrect root hash: expected {:?}, got {:?}",
            expected_root_hash, root_hash
        );
    }

    #[tokio::test]
    async fn test_proof() {
        // Initialize a buffer with random data
        let mut buf = vec![0u8; CHUNK_SIZE];
        rand::thread_rng().fill(buf.as_mut_slice());

        // Create a BMT pool
        let pool = Arc::new(Pool::new(1).await);
        let mut hasher = pool.get_hasher().await.unwrap();
        let _ = hasher.set_span(buf.len().try_into().unwrap());
        hasher.write(&buf).unwrap();

        let mut root_hash = [0u8; SEGMENT_SIZE];
        hasher.hash(&mut root_hash);

        // Iterate over all segments and test proofs
        for i in 0..CHUNK_SIZE / SEGMENT_SIZE {
            let segment_index = i;

            let proof = hasher
                .proof(segment_index)
                .expect("Failed to generate proof");

            // Create a new hasher for verification
            let verified_root = Hasher::verify(segment_index, proof).unwrap();

            assert_eq!(
                root_hash, verified_root,
                "Incorrect hash: expected {:?}, got {:?}",
                root_hash, verified_root
            );
        }
    }
}
