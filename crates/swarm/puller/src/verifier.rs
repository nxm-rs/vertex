//! Interim signature-only [`PullChunkVerifier`].
//!
//! Recovers the stamp signer over the chunk address, proving the stamp signature
//! is well-formed and bound to the chunk. The full check also resolves the batch
//! and confirms on-chain funding; that plugs in once the batch-store verifier
//! lands. Until then this admits any structurally-sound, correctly-signed chunk.

use vertex_swarm_api::{PullChunkVerifier, VerifyError};
use vertex_swarm_primitives::StampedChunk;

/// Signature-only verifier: recovers the stamp signer over the chunk address.
#[derive(Debug, Clone, Copy, Default)]
pub struct SignatureVerifier;

impl PullChunkVerifier for SignatureVerifier {
    fn verify(&self, chunk: &StampedChunk) -> Result<(), VerifyError> {
        chunk
            .stamp()
            .recover_signer(chunk.address())
            .map(|_| ())
            .map_err(|_| VerifyError::InvalidSignature)
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "test assertions over known-bounds fixtures"
)]
mod tests {
    use super::*;
    use alloy_primitives::B256;
    use nectar_postage::Stamp;
    use nectar_primitives::ContentChunk;
    use vertex_swarm_primitives::StampedChunk;

    /// A stamp whose signature does not recover to a valid signer (zeroed sig).
    fn unsigned_chunk() -> StampedChunk {
        let chunk = ContentChunk::new(&b"verify-test"[..]).unwrap();
        let mut raw = [0u8; 65];
        raw[64] = 27;
        let sig = alloy_primitives::Signature::try_from(&raw[..]).unwrap();
        let stamp = Stamp::new(B256::repeat_byte(0xaa), 3, 7, 42, sig);
        StampedChunk::new(chunk.into(), stamp)
    }

    #[test]
    fn malformed_signature_is_rejected() {
        // A zero signature recovers no signer; the verifier rejects it.
        let err = SignatureVerifier.verify(&unsigned_chunk()).unwrap_err();
        assert!(matches!(err, VerifyError::InvalidSignature));
    }
}
