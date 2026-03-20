//! Gossip check errors and verification failure types.

use vertex_net_dialer::EnqueueError;

/// Why a gossiped peer was rejected before verification.
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
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

impl GossipCheckError {
    vertex_metrics::impl_record_error!("topology_gossip_rejected_total");
}

/// Verification failure after handshake.
#[derive(Debug, Clone, Copy, strum::Display, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub(super) enum VerificationFailure {
    /// The gossiped multiaddr is not in the verified peer's address list.
    MultiAddrNotInPeer,
}

impl VerificationFailure {
    vertex_metrics::impl_record_error!("topology_gossip_verification_failed_total");
}
