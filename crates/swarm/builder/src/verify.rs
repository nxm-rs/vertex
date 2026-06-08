//! Config-gated verification wrapper for downloaded chunks.
//!
//! Content integrity is cheap to check once the bytes are in hand: reconstruct
//! the chunk from the returned data and confirm its derived address matches the
//! requested one. This wrapper makes that check the default. Stamp verification
//! is a separate, opt-in concern and is off by default.
//!
//! [`VerifyingChunkProvider`] wraps any [`SwarmChunkProvider`] and applies the
//! configured checks to every retrieved chunk before handing it back to the
//! caller.

use async_trait::async_trait;
use nectar_primitives::{DefaultContentChunk, DefaultSingleOwnerChunk};
use vertex_swarm_api::{
    Chunk, ChunkAddress, ChunkRetrievalResult, SwarmChunkProvider, SwarmError, SwarmResult,
};

/// Which checks the [`VerifyingChunkProvider`] applies to retrieved chunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkVerifyConfig {
    /// Reconstruct the chunk from its bytes and confirm the derived address
    /// matches the requested address. On by default: if the chunk verifies we
    /// got the integrity guarantee for free.
    pub verify_content: bool,
    /// Verify the postage stamp signature against the chunk. Off by default;
    /// callers that need delivery guarantees can opt in.
    pub verify_stamp: bool,
}

impl Default for ChunkVerifyConfig {
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

/// Confirm the retrieved bytes derive the requested `address`.
///
/// The retrieval wire form carries no chunk-type tag, so the bytes are
/// interpreted by trying each concrete chunk type and keeping the first whose
/// derived address matches `address`. A content-addressed chunk is tried first
/// (the common case), then a single-owner chunk.
fn verify_content(data: &bytes::Bytes, address: &ChunkAddress) -> SwarmResult<()> {
    if let Ok(chunk) = DefaultContentChunk::try_from(data.clone())
        && chunk.verify(address).is_ok()
    {
        return Ok(());
    }

    if let Ok(chunk) = DefaultSingleOwnerChunk::try_from(data.clone())
        && chunk.verify(address).is_ok()
    {
        return Ok(());
    }

    Err(SwarmError::InvalidChunk {
        address: Some(*address),
        reason: "retrieved chunk failed content verification".to_string(),
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
            verify_content(&result.data, address)?;
        }

        if self.config.verify_stamp {
            // A stamp-verification helper that checks the signature against the
            // chunk is not yet wired through here. Rather than silently passing
            // an unchecked stamp, surface that the requested check is
            // unavailable.
            return Err(SwarmError::InvalidSignature {
                chunk_address: *address,
                reason: "stamp verification unavailable".to_string(),
            });
        }

        Ok(result)
    }

    fn has_chunk(&self, address: &ChunkAddress) -> bool {
        self.inner.has_chunk(address)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use bytes::Bytes;

    /// In-test provider returning canned bytes and stamp for any request.
    struct MockProvider {
        data: Bytes,
        served_by: vertex_swarm_api::OverlayAddress,
    }

    #[async_trait]
    impl SwarmChunkProvider for MockProvider {
        async fn retrieve_chunk(
            &self,
            _address: &ChunkAddress,
        ) -> SwarmResult<ChunkRetrievalResult> {
            Ok(ChunkRetrievalResult {
                data: self.data.clone(),
                stamp: Bytes::new(),
                served_by: self.served_by,
            })
        }

        fn has_chunk(&self, _address: &ChunkAddress) -> bool {
            true
        }
    }

    fn content_chunk() -> DefaultContentChunk {
        DefaultContentChunk::new(&b"hello swarm"[..]).unwrap()
    }

    #[tokio::test]
    async fn valid_content_chunk_verifies() {
        let chunk = content_chunk();
        let address = *chunk.address();
        let data = Bytes::from(chunk);

        let provider = VerifyingChunkProvider::new(
            MockProvider {
                data,
                served_by: Default::default(),
            },
            ChunkVerifyConfig::default(),
        );

        let result = provider.retrieve_chunk(&address).await;
        assert!(result.is_ok(), "valid chunk should verify: {result:?}");
    }

    #[tokio::test]
    async fn corrupted_data_fails_verification() {
        let chunk = content_chunk();
        let address = *chunk.address();
        let mut bytes = Bytes::from(chunk).to_vec();
        // Flip a byte in the payload so the derived address no longer matches.
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;

        let provider = VerifyingChunkProvider::new(
            MockProvider {
                data: Bytes::from(bytes),
                served_by: Default::default(),
            },
            ChunkVerifyConfig::default(),
        );

        let result = provider.retrieve_chunk(&address).await;
        assert!(
            matches!(result, Err(SwarmError::InvalidChunk { .. })),
            "corrupted chunk should fail verification: {result:?}"
        );
    }

    #[tokio::test]
    async fn content_check_disabled_passes_corrupted_data() {
        let chunk = content_chunk();
        let address = *chunk.address();
        let mut bytes = Bytes::from(chunk).to_vec();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;

        let provider = VerifyingChunkProvider::new(
            MockProvider {
                data: Bytes::from(bytes),
                served_by: Default::default(),
            },
            ChunkVerifyConfig {
                verify_content: false,
                verify_stamp: false,
            },
        );

        let result = provider.retrieve_chunk(&address).await;
        assert!(result.is_ok(), "content check off should not verify");
    }

    #[tokio::test]
    async fn stamp_verification_reports_unavailable() {
        let chunk = content_chunk();
        let address = *chunk.address();
        let data = Bytes::from(chunk);

        let provider = VerifyingChunkProvider::new(
            MockProvider {
                data,
                served_by: Default::default(),
            },
            ChunkVerifyConfig {
                verify_content: true,
                verify_stamp: true,
            },
        );

        let result = provider.retrieve_chunk(&address).await;
        assert!(
            matches!(result, Err(SwarmError::InvalidSignature { .. })),
            "stamp verification should report unavailable: {result:?}"
        );
    }
}
