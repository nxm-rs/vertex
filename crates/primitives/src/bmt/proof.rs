use crate::bmt::{Hasher, Segment, Span, SEGMENT_PAIR_SIZE};
use crate::{CHUNK_SIZE, SEGMENT_SIZE};
use anyhow::{anyhow, Result};

/// Represents a Merkle proof of segment
#[derive(Debug, Clone)]
pub struct Proof {
    pub prove_segment: Segment,
    pub proof_segments: Vec<Segment>,
    pub span: Span,
    pub index: usize,
}

pub trait Prover {
    /// Returns the inclusion proof of the i-th data segment
    async fn proof(&self, i: usize) -> Result<Proof>;
    /// Verifies the proof and returns the root hash derived from it
    async fn verify(&self, i: usize, proof: Proof) -> Result<Segment>;
}

impl Prover for Hasher {
    async fn proof(&self, i: usize) -> Result<Proof> {
        let tree = self.treee;

        // Calculate the starting offset of the segment pair
        let segment_index = i / 2;
        let offset = segment_index * SEGMENT_PAIR_SIZE;

        // Directly index into the buffer to get the two segments
        let section = &tree.buffset..offset + SEGMENT_PAIR_SIZE];
        let (segment, first_segment_sister) = if i % 2 == 0 {
            (
                section[..SEGMENT_SIZE]
                    .try_into()
                    .expect("Slice size mismatch"),
                section[SEGMENT_SIZE..]
                    .try_into()
                    .expect("Slice size mismatch"),
            )
        } else {
            (
                section[SEGMENT_SIZE..]
                    .try_into()
                    .expect("Slice size mismatch"),
                section[..SEGMENT_SIZE]
                    .try_into()
                    .expect("Slice size mismatch"),
            )
        };

        let mut proof_segments = vec![first_segment_sister];

        let n = tree.leaves[segment_index].lock();
        let mut node_ref = n.parent();
        drop(n);

<F6>        let mut level = 1;
        while let Some(current_node_ref) = node_ref {
            let current_node = current_node_ref.lock();

            proof_segments.push(
                current_node
                    .segment(!is_left(level, segment_index))
                    .unwrap(),
            );
            node_ref = current_node.parent();
            level += 1;
        }

        Ok(Proof {
            prove_segment: segment,
            proof_segments,
            span: self.span,
            index: i,
        })
    }

    async fn verify(&self, i: usize, proof: Proof) -> Result<Segment> {
        if proof.proof_segments.is_empty() {
            return Err(anyhow!("Proof segments are empty"));
        }

        // Combine the first segment with its first proof segment
        let hasher = alloy_primitives::keccak256;
        let mut hash = if i % 2 == 0 {
            hasher(
                [
                    proof.prove_segment.as_slice(),
                    proof.proof_segments[0].as_slice(),
                ]
                .concat(),
            )
        } else {
            hasher(
                [
                    proof.proof_segments[0].as_slice(),
                    proof.prove_segment.as_slice(),
                ]
                .concat(),
            )
        };

        let tree = self.treee.lock();

        let segment_index = i / 2;
        let n = tree.leaves[segment_index].lock();
        let mut node_ref = n.parent();
        drop(n);

        // Traverse up the proof segments to compute the root hash
        let mut level = 1;
        for sister in &proof.proof_segments[1..] {
            if let Some(current_node_ref) = node_ref {
                let current_node = current_node_ref.lock();
                hash = if is_left(level, segment_index) {
                    hasher([hash.as_slice(), sister.as_slice()].concat())
                } else {
                    hasher([sister.as_slice(), hash.as_slice()].concat())
                };
                node_ref = current_node.parent();
                level += 1;
            }
        }

        // Combine the final hash with the span to compute the root hash
        let root_hash = hasher([proof.span.as_slice(), hash.as_slice()].concat());
        Ok(*root_hash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bmt::{length_to_span, pool::PooledHasher, Pool};
    use alloy_primitives::hex;
    use rand::Rng;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    #[tokio::test]
    async fn test_proof_correctness() {
        let mut buf = vec![0u8; CHUNK_SIZE];
        let data = b"hello world";
        buf[..data.len()].copy_from_slice(data);

        let pool = Arc::new(Pool::new(1).await);
        let prover = Arc::new(Mutex::new(pool.get_hasher().await.unwrap()));
        let mut hasher = prover.lock().await;
        hasher.set_header_u64(buf.len() as u64);
        hasher.write(&buf).unwrap();

        let _ = hasher.hash().await.expect("Unable to compute root hash");
        drop(hasher);

        let verify_segments = |expected: &[&str], found: &[Segment]| {
            assert_eq!(
                expected.len(),
                found.len(),
                "Incorrect number of proof segments"
            );

            for (exp, found_segment) in expected.iter().zip(found.iter()) {
                let decoded = hex::decode(exp).expect("Invalid hex encoding");
                assert_eq!(&decoded, found_segment, "Incorrect segment in proof");
            }
        };

        // Test leftmost segment
        {
            let proof = prover.lock().await.proof(0).await.unwrap();
            let expected_segments = [
                "0000000000000000000000000000000000000000000000000000000000000000",
                "ad3228b676f7d3cd4284a5443f17f1962b36e491b30a40b2405849e597ba5fb5",
                "b4c11951957c6f8f642c4af61cd6b24640fec6dc7fc607ee8206a99e92410d30",
                "21ddb9a356815c3fac1026b6dec5df3124afbadb485c9ba5a3e3398a04b7ba85",
                "e58769b32a1beaf1ea27375a44095a0d1fb664ce2dd358e7fcbfb78c26a19344",
                "0eb01ebfc9ed27500cd4dfc979272d1f0913cc9f66540d7e8005811109e1cf2d",
                "887c22bd8750d34016ac3c66b5ff102dacdd73f6b014e710b51e8022af9a1968",
            ];

            verify_segments(&expected_segments, &proof.proof_segments);
            assert_eq!(
                &buf[..SEGMENT_SIZE],
                &proof.prove_segment,
                "Incorrect leftmost segment"
            );
            assert_eq!(
                proof.span,
                length_to_span(CHUNK_SIZE as u64),
                "Incorrect span for leftmost proof"
            );
        }

        // Test rightmost segment
        {
            let proof = prover.lock().await.proof(127).await.unwrap();
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
                proof.span,
                length_to_span(CHUNK_SIZE as u64),
                "Incorrect span for rightmost proof"
            );
        }

        // Test middle segment
        {
            let proof = prover.lock().await.proof(64).await.unwrap();
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
                proof.span,
                length_to_span(CHUNK_SIZE as u64),
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

        let segments: Vec<Segment> = segment_strings
            .iter()
            .map(|s| {
                hex::decode(s)
                    .expect("Invalid hex encoding")
                    .try_into()
                    .expect("Slice size mismatch")
            })
            .collect();

        let mut buf = vec![0u8; CHUNK_SIZE];
        let data = b"hello world";
        buf[..data.len()].copy_from_slice(data);

        let pool = Arc::new(Pool::new(1).await);
        let mut prover = pool.get_hasher().await.unwrap();
        prover.set_header_u64(buf.len() as u64);
        prover.write(&buf).unwrap();

        let proof = Proof {
            prove_segment: buf[64 * SEGMENT_SIZE..65 * SEGMENT_SIZE]
                .try_into()
                .expect("Slice size mismatch"),
            proof_segments: segments,
            span: length_to_span(4096),
            index: 64,
        };

        let root_hash = prover.verify(64, proof).await.unwrap();
        let expected_root_hash = prover.hash().await.unwrap();

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
        let pool = Arc::new(Pool::new(16).await);
        let prover = Arc::new(Mutex::new(pool.get_hasher().await.unwrap()));
        let mut hasher = prover.lock().await;
        hasher.set_header_u64(buf.len().try_into().unwrap());
        hasher.write(&buf).unwrap();

        let root_hash = hasher.hash().await.expect("Unable to root hash");
        drop(hasher);

        // Iterate over all segments and test proofs
        for i in 0..CHUNK_SIZE / SEGMENT_SIZE {
            let segment_index = i;

            let prover = prover.clone();
            let pool = pool.clone();
            tokio::task::spawn(async move {
                let prover = prover.lock().await;
                let proof = prover
                    .proof(segment_index)
                    .await
                    .expect("Failed to generate proof");
                drop(prover);

                // Create a new hasher for verification
                let verifier = pool.get_hasher().await.unwrap();

                let verified_root = verifier
                    .verify(segment_index, proof)
                    .await
                    .expect("Failed to verify proof");

                // Ensure the root hash matches
                assert_eq!(
                    root_hash, verified_root,
                    "Incorrect hash: expected {:?}, got {:?}",
                    root_hash, verified_root
                );
            })
            .await
            .expect("Test task failed");
        }
    }
}
