//! Hive protocol validation errors.

use metrics::counter;
use strum::IntoStaticStr;

use core::array::TryFromSliceError;

/// Per-peer validation failure reasons.
#[derive(Debug, thiserror::Error, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
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

impl ValidationFailure {
    pub(crate) fn record(&self) {
        let reason: &'static str = self.into();
        counter!("hive_peer_validation_failures_total", "reason" => reason).increment(1);
    }
}
