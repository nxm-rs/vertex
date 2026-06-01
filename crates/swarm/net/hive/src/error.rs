//! Hive protocol validation errors.
//!
//! Retained as a public type so downstream callers compiled against earlier
//! versions of the crate continue to build. The active validation path now
//! lives in [`crate::verifier`] and reports failures as [`crate::HiveRejection`]
//! instead.

use strum::IntoStaticStr;

use core::array::TryFromSliceError;

/// Per-peer validation failure reasons (legacy surface).
///
/// New code should consume [`crate::HiveRejection`] from the
/// [`crate::HiveVerifier`] trait.
#[derive(Debug, thiserror::Error, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum ValidationFailure {
    #[error("invalid overlay length: {0}")]
    OverlayLength(TryFromSliceError),
    #[error("invalid signature: {0}")]
    SignatureFormat(#[from] alloy_primitives::SignatureError),
    #[error("invalid nonce length: {0}")]
    NonceLength(TryFromSliceError),
    #[error("peer validation failed: {0}")]
    PeerValidation(#[from] vertex_swarm_peer::error::SwarmPeerError),
    #[error("peer is own overlay")]
    SelfOverlay,
    #[error("multiaddrs missing /p2p/ component")]
    MissingPeerId,
}
