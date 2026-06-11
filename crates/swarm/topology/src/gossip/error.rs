//! Gossip intake rejection reasons.

/// Why a gossiped record was rejected at intake.
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub(super) enum GossipCheckError {
    #[error("no multiaddrs")]
    NoMultiaddrs,
    #[error("no peer ID")]
    NoPeerId,
    #[error("gossiper rate limited")]
    GossiperRateLimited,
    #[error("record cooldown active")]
    CooldownActive,
}

impl GossipCheckError {
    vertex_metrics::impl_record_error!("topology_gossip_rejected_total");
}
