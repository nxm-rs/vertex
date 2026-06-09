//! A chunk paired with the postage stamp that authorizes its storage.

use nectar_postage::Stamp;
use nectar_primitives::{AnyChunk, ChunkAddress, ContentChunk, SingleOwnerChunk, bytes::Bytes};

/// Error rebuilding a chunk from its wire bytes and expected address.
///
/// Reconstructing an [`AnyChunk`] from raw bytes is ambiguous without the
/// address: a [`ContentChunk`] parse almost always succeeds structurally (span
/// plus arbitrary payload), so the expected address is the disambiguator. The
/// chunk is whichever variant parses *and* hashes to the expected address; if
/// neither does, the bytes do not match the address.
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum ReconstructError {
    /// The bytes did not parse as any known chunk type whose address matches the
    /// expected address.
    #[error("chunk bytes do not match expected address {expected}")]
    AddressMismatch {
        /// The address the chunk was expected to have.
        expected: ChunkAddress,
    },
}

/// A chunk together with its postage stamp.
///
/// A retrieval, a pushsync delivery, and an upload all move a *chunk plus its
/// proof of payment* as one unit. [`AnyChunk`] holds the chunk bytes but carries
/// no stamp, so this pairing is the cohesive value that flows across the node's
/// command, event, and provider boundaries instead of two loose fields (or raw
/// `Bytes`).
///
/// The address is the chunk's own address; [`address`](Self::address) delegates
/// to it.
// TODO(nectar): migrate StampedChunk upstream once the postage stamp and chunk
// types live together there; it is vertex-only for now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StampedChunk {
    chunk: AnyChunk,
    stamp: Stamp,
}

impl StampedChunk {
    /// Pair a chunk with its stamp.
    #[inline]
    #[must_use]
    pub fn new(chunk: AnyChunk, stamp: Stamp) -> Self {
        Self { chunk, stamp }
    }

    /// The chunk.
    #[inline]
    #[must_use]
    pub fn chunk(&self) -> &AnyChunk {
        &self.chunk
    }

    /// The postage stamp.
    #[inline]
    #[must_use]
    pub fn stamp(&self) -> &Stamp {
        &self.stamp
    }

    /// The chunk's address (delegates to the chunk).
    #[inline]
    #[must_use]
    pub fn address(&self) -> &ChunkAddress {
        self.chunk.address()
    }

    /// Rebuild a stamped chunk from its wire bytes, expected address, and stamp.
    ///
    /// The expected address disambiguates the chunk variant: a [`ContentChunk`]
    /// parse almost always succeeds structurally, so the chunk is accepted only
    /// if it also hashes to `expected`. Tries content first, then single-owner,
    /// matching each against `expected`. A lying address makes both attempts
    /// fail, so the address is self-validating against the bytes.
    pub fn reconstruct(
        expected: ChunkAddress,
        data: Bytes,
        stamp: Stamp,
    ) -> Result<Self, ReconstructError> {
        let chunk = reconstruct_chunk(expected, data)?;
        Ok(Self::new(chunk, stamp))
    }

    /// Split into the chunk and its stamp.
    #[inline]
    #[must_use]
    pub fn into_parts(self) -> (AnyChunk, Stamp) {
        (self.chunk, self.stamp)
    }
}

/// Rebuild an [`AnyChunk`] from wire bytes given the expected address.
///
/// See [`StampedChunk::reconstruct`] for the disambiguation rationale.
pub fn reconstruct_chunk(
    expected: ChunkAddress,
    data: Bytes,
) -> Result<AnyChunk, ReconstructError> {
    use nectar_primitives::Chunk;

    if let Ok(content) = ContentChunk::try_from(data.clone())
        && *content.address() == expected
    {
        return Ok(AnyChunk::Content(content));
    }
    if let Ok(soc) = SingleOwnerChunk::try_from(data)
        && *soc.address() == expected
    {
        return Ok(AnyChunk::SingleOwner(soc));
    }
    Err(ReconstructError::AddressMismatch { expected })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use alloy_primitives::{B256, Signature};
    use alloy_signer_local::PrivateKeySigner;
    use nectar_primitives::{Chunk, ContentChunk, SingleOwnerChunk};

    use super::*;

    fn test_stamp() -> Stamp {
        let sig = Signature::from_raw(&[1u8; 65]).expect("valid signature");
        Stamp::new(B256::repeat_byte(0xaa), 3, 7, 42, sig)
    }

    fn content_chunk() -> ContentChunk {
        ContentChunk::new(&b"hello swarm"[..]).expect("valid content chunk")
    }

    fn single_owner_chunk() -> SingleOwnerChunk {
        let signer = PrivateKeySigner::from_bytes(&B256::repeat_byte(0x11)).expect("valid signer");
        SingleOwnerChunk::new(B256::repeat_byte(0x22), &b"soc payload"[..], &signer)
            .expect("valid soc")
    }

    #[test]
    fn into_parts_round_trips_the_fields() {
        let chunk: AnyChunk = content_chunk().into();
        let stamp = test_stamp();
        let stamped = StampedChunk::new(chunk.clone(), stamp.clone());
        assert_eq!(stamped.address(), chunk.address());
        let (got_chunk, got_stamp) = stamped.into_parts();
        assert_eq!(got_chunk, chunk);
        assert_eq!(got_stamp, stamp);
    }

    #[test]
    fn reconstruct_content_chunk_is_identity() {
        let chunk = content_chunk();
        let address = *chunk.address();
        let data = Bytes::from(chunk);
        let rebuilt = StampedChunk::reconstruct(address, data.clone(), test_stamp())
            .expect("content reconstruct");
        assert!(rebuilt.chunk().is_content());
        assert_eq!(*rebuilt.address(), address);
        // Encode is byte-identical to the original wire data.
        assert_eq!(rebuilt.into_parts().0.into_bytes(), data);
    }

    #[test]
    fn reconstruct_single_owner_chunk_is_identity() {
        let chunk = single_owner_chunk();
        let address = *chunk.address();
        let data = Bytes::from(chunk);
        let rebuilt = StampedChunk::reconstruct(address, data.clone(), test_stamp())
            .expect("soc reconstruct");
        assert!(rebuilt.chunk().is_single_owner());
        assert_eq!(*rebuilt.address(), address);
        assert_eq!(rebuilt.into_parts().0.into_bytes(), data);
    }

    #[test]
    fn reconstruct_rejects_wrong_address() {
        let chunk = content_chunk();
        let data = Bytes::from(chunk);
        let wrong = ChunkAddress::new([0xff; 32]);
        let err = StampedChunk::reconstruct(wrong, data, test_stamp())
            .expect_err("wrong address must fail");
        assert!(matches!(err, ReconstructError::AddressMismatch { .. }));
    }
}
