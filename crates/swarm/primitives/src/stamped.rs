//! A chunk paired with the postage stamp that authorizes its storage.

use nectar_postage::Stamp;
use nectar_primitives::{AnyChunk, ChunkAddress};

/// A chunk together with its postage stamp.
pub use nectar_postage::StampedChunk;

/// Serve-time verification gate on [`StampedChunk`], as an extension trait
/// because the orphan rule forbids an inherent method on the upstream type.
pub trait StampedChunkExt {
    /// Prove this chunk answers a request for `requested`, consuming it into a
    /// [`VerifiedStampedChunk`].
    ///
    /// The chunk's address is derived from its own bytes, so an equal address
    /// means the bytes are exactly the ones the requester asked for. Returns the
    /// chunk unchanged in [`Err`] on a mismatch so the caller can treat it as a
    /// miss without losing the value (boxed because a [`StampedChunk`] is large).
    fn verify_answers(
        self,
        requested: ChunkAddress,
    ) -> Result<VerifiedStampedChunk, Box<StampedChunk>>;
}

impl StampedChunkExt for StampedChunk {
    fn verify_answers(
        self,
        requested: ChunkAddress,
    ) -> Result<VerifiedStampedChunk, Box<StampedChunk>> {
        if *self.address() == requested {
            Ok(VerifiedStampedChunk(self))
        } else {
            Err(Box::new(self))
        }
    }
}

/// A [`StampedChunk`] proven to answer a specific request.
///
/// Constructed only by [`StampedChunkExt::verify_answers`]. A responder accepts
/// only this type, so an unverified chunk cannot be sent down the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedStampedChunk(StampedChunk);

impl VerifiedStampedChunk {
    #[inline]
    #[must_use]
    pub fn address(&self) -> &ChunkAddress {
        self.0.address()
    }

    #[inline]
    #[must_use]
    pub fn stamped(&self) -> &StampedChunk {
        &self.0
    }

    #[inline]
    #[must_use]
    pub fn into_inner(self) -> StampedChunk {
        self.0
    }
}

/// A chunk paired with an *optional* postage stamp, as the local cache holds it.
///
/// A content chunk (CAC) is immutable and cached stampless (`stamp == None`). A
/// single-owner chunk (SOC) is mutable at a fixed address, ordered by the
/// stamp's signed timestamp, so a cached SOC always carries a stamp; the
/// retrieval path never caches a SOC since a stampless one has no version signal.
/// [`StampedChunk`] remains the always-stamped currency on the network paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedChunk {
    chunk: AnyChunk,
    stamp: Option<Stamp>,
}

impl CachedChunk {
    #[inline]
    #[must_use]
    pub fn new(chunk: AnyChunk, stamp: Option<Stamp>) -> Self {
        Self { chunk, stamp }
    }

    #[inline]
    #[must_use]
    pub fn chunk(&self) -> &AnyChunk {
        &self.chunk
    }

    #[inline]
    #[must_use]
    pub fn stamp(&self) -> Option<&Stamp> {
        self.stamp.as_ref()
    }

    #[inline]
    #[must_use]
    pub fn address(&self) -> &ChunkAddress {
        self.chunk.address()
    }

    #[inline]
    #[must_use]
    pub fn into_parts(self) -> (AnyChunk, Option<Stamp>) {
        (self.chunk, self.stamp)
    }
}

impl From<StampedChunk> for CachedChunk {
    fn from(stamped: StampedChunk) -> Self {
        let (chunk, stamp) = stamped.into_parts();
        Self::new(chunk, Some(stamp))
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{B256, Signature};
    use alloy_signer_local::PrivateKeySigner;
    use nectar_postage::{Stamp, StampError};
    use nectar_primitives::{Chunk, ContentChunk, SingleOwnerChunk, bytes::Bytes};

    use super::*;

    // Pins the default body size for `reconstruct` call sites, which take no
    // argument that fixes the generic.
    type DefaultStampedChunk = StampedChunk;

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
        let rebuilt = DefaultStampedChunk::reconstruct(address, data.clone(), test_stamp())
            .expect("content reconstruct");
        assert!(rebuilt.chunk().is_content());
        assert_eq!(*rebuilt.address(), address);
        // Re-encode is byte-identical to the original wire data.
        assert_eq!(rebuilt.into_parts().0.into_bytes(), data);
    }

    #[test]
    fn reconstruct_single_owner_chunk_is_identity() {
        let chunk = single_owner_chunk();
        let address = *chunk.address();
        let data = Bytes::from(chunk);
        let rebuilt = DefaultStampedChunk::reconstruct(address, data.clone(), test_stamp())
            .expect("soc reconstruct");
        assert!(rebuilt.chunk().is_single_owner());
        assert_eq!(*rebuilt.address(), address);
        assert_eq!(rebuilt.into_parts().0.into_bytes(), data);
    }

    #[test]
    fn verify_answers_accepts_matching_address() {
        let chunk: AnyChunk = content_chunk().into();
        let address = *chunk.address();
        let stamped = StampedChunk::new(chunk, test_stamp());
        let verified = stamped
            .verify_answers(address)
            .expect("matching address verifies");
        assert_eq!(*verified.address(), address);
    }

    #[test]
    fn verify_answers_rejects_mismatched_address() {
        let chunk: AnyChunk = content_chunk().into();
        let stamped = StampedChunk::new(chunk, test_stamp());
        let wrong = ChunkAddress::new([0xff; 32]);
        let returned = stamped
            .clone()
            .verify_answers(wrong)
            .expect_err("wrong address must fail");
        assert_eq!(*returned, stamped);
    }

    #[test]
    fn reconstruct_rejects_wrong_address() {
        let chunk = content_chunk();
        let data = Bytes::from(chunk);
        let wrong = ChunkAddress::new([0xff; 32]);
        let err = DefaultStampedChunk::reconstruct(wrong, data, test_stamp())
            .expect_err("wrong address must fail");
        assert!(matches!(err, StampError::Chunk(_)));
    }
}
