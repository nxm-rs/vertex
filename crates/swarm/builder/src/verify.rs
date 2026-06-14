//! Config-gated verification wrapper for downloaded chunks.
//!
//! Content integrity is established before a chunk reaches this wrapper: the
//! retrieval codec reconstructs the chunk from the wire bytes and accepts it only
//! if it hashes to the requested address, so [`SwarmChunkProvider::retrieve_chunk`]
//! cannot return a chunk whose address disagrees with the request. The expensive
//! reconstruction-and-hash work therefore no longer lives here.
//!
//! What is left for this wrapper is two cheap residual checks: a defensive
//! re-assertion that the returned chunk's address matches the request (on by
//! default, effectively free now that retrieval guarantees it), and postage stamp
//! signer recovery (off by default, the part reconstruction does not cover).
//!
//! [`VerifyingChunkProvider`] wraps any [`SwarmChunkProvider`] and applies the
//! configured checks to every retrieved chunk before handing it back to the
//! caller.

use async_trait::async_trait;
use vertex_swarm_api::{
    AnyChunk, ChunkAddress, ChunkRetrievalResult, PushReceipt, Stamp, StampedChunk,
    SwarmChunkProvider, SwarmChunkSender, SwarmError, SwarmResult,
};

/// Which checks the [`VerifyingChunkProvider`] applies to retrieved chunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkVerifyConfig {
    /// Re-assert that the returned chunk's address matches the requested address.
    /// On by default. Content integrity is already enforced during retrieval
    /// decode, so this is a cheap defensive equality check, not a re-hash.
    pub verify_content: bool,
    /// Recover the postage stamp signer for the chunk address. Off by default;
    /// callers that need to confirm the stamp signature is well-formed can opt in.
    /// This recovers the signer but does not check batch validity on-chain, which
    /// is the storer's concern.
    pub verify_stamp: bool,
}

impl Default for ChunkVerifyConfig {
    // Hand-written rather than derived: the workspace derive set cannot express a
    // field defaulting to `true`. `derive_more` ships no `Default` derive, and no
    // field-level-default crate is a workspace dependency, so this matches the
    // manual `impl Default` used by the sibling configs in this crate.
    fn default() -> Self {
        Self {
            verify_content: true,
            verify_stamp: false,
        }
    }
}

/// Wraps a [`SwarmChunkProvider`] and verifies retrieved chunks per
/// [`ChunkVerifyConfig`].
#[derive(Debug, Clone)]
pub struct VerifyingChunkProvider<P> {
    inner: P,
    config: ChunkVerifyConfig,
}

impl<P> VerifyingChunkProvider<P> {
    /// Wrap `inner`, applying the checks selected by `config`.
    pub fn new(inner: P, config: ChunkVerifyConfig) -> Self {
        Self { inner, config }
    }
}

/// Re-assert that `chunk` carries the requested `address`.
///
/// Retrieval already reconstructs and address-validates the chunk during decode,
/// so this is a defensive equality check that costs a single comparison.
fn verify_content(chunk: &AnyChunk, address: &ChunkAddress) -> SwarmResult<()> {
    if chunk.address() == address {
        return Ok(());
    }

    Err(SwarmError::InvalidChunk {
        address: Some(*address),
        reason: "retrieved chunk address does not match requested address".to_string(),
    })
}

/// Recover the postage stamp signer for the chunk address.
///
/// Confirms the stamp signature is well-formed and recovers cleanly for this
/// chunk. It does not check that the recovered signer owns a valid batch on-chain.
fn verify_stamp(stamp: &Stamp, address: &ChunkAddress) -> SwarmResult<()> {
    stamp
        .recover_signer(address)
        .map(|_| ())
        .map_err(|err| SwarmError::InvalidSignature {
            chunk_address: *address,
            reason: err.to_string(),
        })
}

#[async_trait]
impl<P> SwarmChunkProvider for VerifyingChunkProvider<P>
where
    P: SwarmChunkProvider,
{
    async fn retrieve_chunk(&self, address: &ChunkAddress) -> SwarmResult<ChunkRetrievalResult> {
        let result = self.inner.retrieve_chunk(address).await?;

        if self.config.verify_content {
            verify_content(&result.chunk, address)?;
        }

        // A stampless delivery skips stamp verification: a storer may omit the
        // stamp from the delivery, and address integrity does not depend on it.
        if self.config.verify_stamp
            && let Some(stamp) = &result.stamp
        {
            verify_stamp(stamp, address)?;
        }

        Ok(result)
    }

    fn has_chunk(&self, address: &ChunkAddress) -> bool {
        self.inner.has_chunk(address)
    }
}

/// Uploads bypass the download-side verification and forward straight to the
/// wrapped sender. [`ChunkVerifyConfig`] governs retrieved chunks only; the
/// upload path keeps its own stamp validation in [`SwarmChunkSender::send_chunk`].
#[async_trait]
impl<P> SwarmChunkSender for VerifyingChunkProvider<P>
where
    P: SwarmChunkSender,
{
    async fn send_chunk_unchecked(&self, chunk: StampedChunk) -> SwarmResult<PushReceipt> {
        self.inner.send_chunk_unchecked(chunk).await
    }

    async fn send_chunk(&self, chunk: StampedChunk) -> SwarmResult<PushReceipt> {
        self.inner.send_chunk(chunk).await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use alloy_primitives::B256;
    use alloy_signer::SignerSync;
    use alloy_signer_local::PrivateKeySigner;
    use nectar_postage::{Stamp, StampDigest, StampIndex};
    use nectar_primitives::Nonce;
    use vertex_swarm_api::{AnyChunk, Chunk, ContentChunk, StorageRadius};

    /// In-test provider returning a canned chunk for any request. The stamp is
    /// optional so the stampless retrieval path can be exercised.
    struct MockProvider {
        chunk: AnyChunk,
        stamp: Option<Stamp>,
        served_by: vertex_swarm_api::OverlayAddress,
    }

    #[async_trait]
    impl SwarmChunkProvider for MockProvider {
        async fn retrieve_chunk(
            &self,
            _address: &ChunkAddress,
        ) -> SwarmResult<ChunkRetrievalResult> {
            Ok(ChunkRetrievalResult {
                chunk: self.chunk.clone(),
                stamp: self.stamp.clone(),
                served_by: self.served_by,
            })
        }

        fn has_chunk(&self, _address: &ChunkAddress) -> bool {
            true
        }
    }

    #[async_trait]
    impl SwarmChunkSender for MockProvider {
        async fn send_chunk_unchecked(&self, _chunk: StampedChunk) -> SwarmResult<PushReceipt> {
            Ok(PushReceipt {
                storer: self.served_by,
                signature: alloy_primitives::Signature::from_raw(&[1u8; 65]).unwrap(),
                nonce: Nonce::new([0u8; 32]),
                storage_radius: StorageRadius::ZERO,
            })
        }

        async fn send_chunk(&self, chunk: StampedChunk) -> SwarmResult<PushReceipt> {
            self.send_chunk_unchecked(chunk).await
        }
    }

    fn content_chunk() -> ContentChunk {
        ContentChunk::new(&b"hello swarm"[..]).unwrap()
    }

    /// A stamp whose EIP-191 signature recovers cleanly for `address`.
    fn valid_stamp(address: &ChunkAddress) -> Stamp {
        let signer = PrivateKeySigner::from_bytes(&B256::repeat_byte(0x11)).unwrap();
        let batch = B256::repeat_byte(0x22);
        let index = StampIndex::new(0, 0);
        let timestamp = 12345u64;
        let prehash = StampDigest::new(*address, batch, index, timestamp).to_prehash();
        let sig = signer.sign_message_sync(prehash.as_slice()).unwrap();
        Stamp::with_index(batch, index, timestamp, sig)
    }

    /// A stamp with an unrecoverable (all-zero) signature.
    fn malformed_stamp() -> Stamp {
        let sig = alloy_primitives::Signature::from_raw(&[0u8; 65]).unwrap();
        Stamp::new(B256::repeat_byte(0xaa), 0, 0, 0, sig)
    }

    fn stamped(chunk: ContentChunk, stamp: Stamp) -> StampedChunk {
        StampedChunk::new(AnyChunk::Content(chunk), stamp)
    }

    /// A retrieval-side mock from a stamped chunk: the chunk carries a stamp.
    fn mock_from(stamped: StampedChunk) -> MockProvider {
        let (chunk, stamp) = stamped.into_parts();
        MockProvider {
            chunk,
            stamp: Some(stamp),
            served_by: Default::default(),
        }
    }

    #[tokio::test]
    async fn matching_address_passes_content_check() {
        let chunk = content_chunk();
        let address = *chunk.address();
        let stamped = stamped(chunk, valid_stamp(&address));

        let provider =
            VerifyingChunkProvider::new(mock_from(stamped), ChunkVerifyConfig::default());

        let result = provider.retrieve_chunk(&address).await;
        assert!(result.is_ok(), "matching address should pass: {result:?}");
    }

    #[tokio::test]
    async fn mismatched_address_fails_content_check() {
        let chunk = content_chunk();
        let address = *chunk.address();
        let stamped = stamped(chunk, valid_stamp(&address));
        let wrong = ChunkAddress::new([0xff; 32]);

        let provider =
            VerifyingChunkProvider::new(mock_from(stamped), ChunkVerifyConfig::default());

        let result = provider.retrieve_chunk(&wrong).await;
        assert!(
            matches!(result, Err(SwarmError::InvalidChunk { .. })),
            "mismatched address should fail content check: {result:?}"
        );
    }

    #[tokio::test]
    async fn content_check_disabled_skips_address_assertion() {
        let chunk = content_chunk();
        let address = *chunk.address();
        let stamped = stamped(chunk, valid_stamp(&address));
        let wrong = ChunkAddress::new([0xff; 32]);

        let provider = VerifyingChunkProvider::new(
            mock_from(stamped),
            ChunkVerifyConfig {
                verify_content: false,
                verify_stamp: false,
            },
        );

        let result = provider.retrieve_chunk(&wrong).await;
        assert!(
            result.is_ok(),
            "content check off should not assert address"
        );
    }

    #[tokio::test]
    async fn valid_stamp_recovers() {
        let chunk = content_chunk();
        let address = *chunk.address();
        let stamped = stamped(chunk, valid_stamp(&address));

        let provider = VerifyingChunkProvider::new(
            mock_from(stamped),
            ChunkVerifyConfig {
                verify_content: true,
                verify_stamp: true,
            },
        );

        let result = provider.retrieve_chunk(&address).await;
        assert!(result.is_ok(), "valid stamp should recover: {result:?}");
    }

    #[tokio::test]
    async fn malformed_stamp_fails_signature_check() {
        let chunk = content_chunk();
        let address = *chunk.address();
        let stamped = stamped(chunk, malformed_stamp());

        let provider = VerifyingChunkProvider::new(
            mock_from(stamped),
            ChunkVerifyConfig {
                verify_content: true,
                verify_stamp: true,
            },
        );

        let result = provider.retrieve_chunk(&address).await;
        assert!(
            matches!(result, Err(SwarmError::InvalidSignature { .. })),
            "malformed stamp should fail signature recovery: {result:?}"
        );
    }

    #[tokio::test]
    async fn upload_forwards_to_inner_sender() {
        let chunk = content_chunk();
        let address = *chunk.address();
        let stamped = stamped(chunk, valid_stamp(&address));

        // Download verification settings must not affect the upload path.
        let provider = VerifyingChunkProvider::new(
            mock_from(stamped.clone()),
            ChunkVerifyConfig {
                verify_content: true,
                verify_stamp: true,
            },
        );

        let receipt = provider.send_chunk(stamped).await;
        assert!(
            receipt.is_ok(),
            "upload should forward to inner: {receipt:?}"
        );
    }

    /// A storer omits the stamp on a delivery: with stamp verification on, a
    /// stampless result is accepted (verification is skipped) and returned with
    /// no stamp. This is the interop acceptance on the operator/embedder surface.
    #[tokio::test]
    async fn stampless_delivery_skips_stamp_verification() {
        let chunk = content_chunk();
        let address = *chunk.address();

        let provider = VerifyingChunkProvider::new(
            MockProvider {
                chunk: AnyChunk::Content(chunk),
                stamp: None,
                served_by: Default::default(),
            },
            ChunkVerifyConfig {
                verify_content: true,
                verify_stamp: true,
            },
        );

        let result = provider
            .retrieve_chunk(&address)
            .await
            .expect("a stampless delivery is accepted");
        assert!(result.stamp.is_none(), "the result carries no stamp");
    }
}
