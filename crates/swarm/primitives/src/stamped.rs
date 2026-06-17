//! A chunk paired with the postage stamp that authorizes its storage.

use nectar_postage::Stamp;
use nectar_primitives::{AnyChunk, ChunkAddress};

/// A chunk together with its postage stamp.
///
/// Re-exported from `nectar-postage`: the canonical `StampedChunk` now lives
/// upstream (chunk plus stamp, with the typed/wire codec), so vertex consumes
/// it directly instead of carrying its own copy. The vertex-specific serve-time
/// and cache type-states ([`VerifiedStampedChunk`], [`CachedChunk`]) wrap it,
/// and [`StampedChunkExt`] adds the vertex-only `verify_answers` gate.
pub use nectar_postage::StampedChunk;

/// Vertex-only extensions to nectar's [`StampedChunk`].
///
/// `verify_answers` is a vertex serve-time gate, not a property of the chunk
/// type, so it cannot be an inherent method on the upstream type (the orphan
/// rule). It is expressed as an extension trait instead.
pub trait StampedChunkExt {
    /// Prove this chunk answers a request for `requested`, consuming it into a
    /// [`VerifiedStampedChunk`].
    ///
    /// The chunk's address is derived from its own bytes (the BMT hash for a
    /// content chunk, owner plus id for a single-owner chunk), so an equal
    /// address means the bytes are exactly the ones the requester asked for.
    /// This is the verify-before-the-wire check expressed as a type-state: only
    /// a [`VerifiedStampedChunk`] can be handed to a responder, so a chunk that
    /// does not answer the request cannot be served by construction.
    ///
    /// Returns the chunk unchanged in [`Err`] on a mismatch so the caller can
    /// treat it as a miss without losing the value. The error is boxed because a
    /// [`StampedChunk`] is large (a full chunk payload plus a stamp).
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
/// Constructed only by [`StampedChunkExt::verify_answers`], which checks the
/// chunk's content-derived address against the requested address. A responder
/// accepts only this type, so a chunk that does not match the request can never
/// be sent down the wire: the gate is a compile-time guarantee rather than a
/// runtime check at the send site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedStampedChunk(StampedChunk);

impl VerifiedStampedChunk {
    /// The chunk's address.
    #[inline]
    #[must_use]
    pub fn address(&self) -> &ChunkAddress {
        self.0.address()
    }

    /// Borrow the underlying stamped chunk.
    #[inline]
    #[must_use]
    pub fn stamped(&self) -> &StampedChunk {
        &self.0
    }

    /// Consume into the underlying stamped chunk for sending down the wire.
    #[inline]
    #[must_use]
    pub fn into_inner(self) -> StampedChunk {
        self.0
    }
}

/// A chunk paired with an *optional* postage stamp, as the local cache holds it.
///
/// The cache stores two kinds of entry that differ only in whether a stamp is
/// present:
///
/// - A content chunk (CAC) is immutable: its address is the BMT hash of its
///   content, so a cached copy is valid forever and carries no freshness signal.
///   The retrieval path delivers it stampless (a storer answers a retrieval with
///   the chunk bytes only), and the cache stores it with `stamp == None`.
/// - A single-owner chunk (SOC) is mutable at a fixed address; the cache orders
///   versions by the stamp's signed timestamp, so a cached SOC always carries a
///   stamp (`stamp == Some`). The retrieval path never caches a SOC, since a
///   stampless SOC has no version signal and could serve a stale revision.
///
/// [`StampedChunk`] remains the always-stamped currency on the network paths
/// (pushsync, upload, the stamped reserve). This type is the cache value, where
/// a stampless content chunk is a first-class entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedChunk {
    chunk: AnyChunk,
    stamp: Option<Stamp>,
}

impl CachedChunk {
    /// Pair a chunk with an optional stamp.
    #[inline]
    #[must_use]
    pub fn new(chunk: AnyChunk, stamp: Option<Stamp>) -> Self {
        Self { chunk, stamp }
    }

    /// The chunk.
    #[inline]
    #[must_use]
    pub fn chunk(&self) -> &AnyChunk {
        &self.chunk
    }

    /// The postage stamp, if one was cached with the chunk.
    #[inline]
    #[must_use]
    pub fn stamp(&self) -> Option<&Stamp> {
        self.stamp.as_ref()
    }

    /// The chunk's address (delegates to the chunk).
    #[inline]
    #[must_use]
    pub fn address(&self) -> &ChunkAddress {
        self.chunk.address()
    }

    /// Split into the chunk and its optional stamp.
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

    // `StampedChunk` is generic over the chunk body size with a default; the
    // `reconstruct` constructor takes no argument that pins it, so spell the
    // default-body-size instantiation out for those call sites.
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
        // Encode is byte-identical to the original wire data.
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
        // The value is returned unchanged so the caller can treat it as a miss.
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
