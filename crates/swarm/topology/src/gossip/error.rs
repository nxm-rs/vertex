//! Gossip check errors and verification failure types.

use vertex_net_dialer::EnqueueError;
use vertex_observability::LabelValue;

/// Why a gossiped peer was rejected before verification.
#[derive(Debug, thiserror::Error)]
pub(super) enum GossipCheckError {
    #[error("no multiaddrs")]
    NoMultiaddrs,
    #[error("no peer ID")]
    NoPeerId,
    #[error("gossiper rate limited")]
    GossiperRateLimited,
    #[error(transparent)]
    Enqueue(#[from] EnqueueError),
}

impl LabelValue for GossipCheckError {
    fn label_value(&self) -> &'static str {
        match self {
            Self::NoMultiaddrs => "no_multiaddrs",
            Self::NoPeerId => "no_peer_id",
            Self::GossiperRateLimited => "gossiper_rate_limited",
            Self::Enqueue(e) => e.label_value(),
        }
    }
}

/// Verification failure after handshake.
#[derive(Debug, Clone, Copy, strum::Display, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub(super) enum VerificationFailure {
    /// The gossiped multiaddr is not in the verified peer's address list.
    MultiAddrNotInPeer,
}
