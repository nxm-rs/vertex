//! The two per-round reserve salts.
//!
//! [`SampleAnchor`] and [`ClaimAnchor`] are distinct newtypes over the same
//! on-chain `bytes32` so that the sample-time and claim-time salts cannot be
//! transposed: a swap yields garbage proofs and a lost round, and the type
//! system rejects it at compile time.

use alloy_primitives::B256;

/// The sample-time reserve salt (the first round anchor).
///
/// The `bytes32 currentRoundAnchor` read from `Redistribution.sol`, used as the
/// BMT prefix that keys transformed addresses (via
/// [`transformed_address`](nectar_primitives::AnyChunk::transformed_address))
/// and the transformed (TR) inclusion proof. Fixed-width `B256` because the
/// on-chain anchor is always a `bytes32`; an earlier `&[u8]` shape over-fit to
/// the minimal-length anchors that some test vectors use.
///
/// A distinct type from [`ClaimAnchor`] so the two round salts, which play
/// structurally different roles and must never be transposed, cannot be passed
/// to each other's slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SampleAnchor(B256);

impl SampleAnchor {
    /// Wrap the on-chain sample-time anchor (`bytes32`).
    #[inline]
    #[must_use]
    pub const fn new(anchor: B256) -> Self {
        Self(anchor)
    }

    /// The salt as a fixed-width `bytes32`.
    #[inline]
    #[must_use]
    pub const fn get(self) -> B256 {
        self.0
    }

    /// The raw salt bytes, threaded untouched into the hashing primitives.
    #[inline]
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_slice()
    }
}

impl From<B256> for SampleAnchor {
    fn from(anchor: B256) -> Self {
        Self(anchor)
    }
}

/// The claim-time reserve salt (the second round anchor).
///
/// The `bytes32 currentRoundAnchor`, interpreted big-endian to select the
/// witness slots and the proven segment (see
/// [`witness_indices`](crate::witness_indices)). See [`SampleAnchor`] for why
/// this is a fixed-width `B256` and a distinct type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClaimAnchor(B256);

impl ClaimAnchor {
    /// Wrap the on-chain claim-time anchor (`bytes32`).
    #[inline]
    #[must_use]
    pub const fn new(anchor: B256) -> Self {
        Self(anchor)
    }

    /// The salt as a fixed-width `bytes32`.
    #[inline]
    #[must_use]
    pub const fn get(self) -> B256 {
        self.0
    }

    /// The raw salt bytes.
    #[inline]
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_slice()
    }
}

impl From<B256> for ClaimAnchor {
    fn from(anchor: B256) -> Self {
        Self(anchor)
    }
}
