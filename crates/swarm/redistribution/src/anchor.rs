//! The two per-round reserve salts.
//!
//! [`SampleAnchor`] and [`ClaimAnchor`] are distinct newtypes over the same
//! on-chain `bytes32` so the sample-time and claim-time salts cannot be
//! transposed: swapping them yields garbage proofs and a lost round, rejected at
//! compile time.

use alloy_primitives::B256;

/// Sample-time reserve salt (first round anchor).
///
/// Keys transformed addresses (via
/// [`transformed_address`](nectar_primitives::AnyChunk::transformed_address))
/// and the transformed-address inclusion proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SampleAnchor(B256);

impl SampleAnchor {
    #[inline]
    #[must_use]
    pub const fn new(anchor: B256) -> Self {
        Self(anchor)
    }

    #[inline]
    #[must_use]
    pub const fn get(self) -> B256 {
        self.0
    }

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

/// Claim-time reserve salt (second round anchor).
///
/// Interpreted big-endian to select the witness slots and proven segment (see
/// [`witness_indices`](crate::witness_indices)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClaimAnchor(B256);

impl ClaimAnchor {
    #[inline]
    #[must_use]
    pub const fn new(anchor: B256) -> Self {
        Self(anchor)
    }

    #[inline]
    #[must_use]
    pub const fn get(self) -> B256 {
        self.0
    }

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
