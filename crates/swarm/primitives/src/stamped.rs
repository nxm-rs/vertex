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
    pub fn verify_answers(
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
/// Constructed only by [`StampedChunk::verify_answers`], which checks the
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
        let err = StampedChunk::reconstruct(wrong, data, test_stamp())
            .expect_err("wrong address must fail");
        assert!(matches!(err, ReconstructError::AddressMismatch { .. }));
    }
}
