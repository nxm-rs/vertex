//! CLI arguments for Kademlia routing configuration.
//!
//! TODO: Refactor to use figment layered config instead of manual unwrap_or
//! default handling. Defaults should come from the config layer, not be
//! duplicated in the args-to-config conversion.

use clap::Args;
use serde::{Deserialize, Serialize};

use super::KademliaConfig;

/// Kademlia routing CLI arguments.
#[derive(Debug, Args, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RoutingArgs {
    /// Target peers per bin before saturation.
    #[arg(long = "network.routing.saturation-peers")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saturation_peers: Option<usize>,

    /// Maximum full nodes per bin.
    #[arg(long = "network.routing.high-watermark")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub high_watermark: Option<usize>,

    /// Slots reserved for light nodes per bin.
    #[arg(long = "network.routing.client-reserved-slots")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_reserved_slots: Option<usize>,

    /// Minimum peers per bin for depth calculation.
    #[arg(long = "network.routing.low-watermark")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub low_watermark: Option<usize>,

    /// Max connection attempts before peer removal.
    #[arg(long = "network.routing.max-connect-attempts")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_connect_attempts: Option<usize>,

    /// Max connection attempts for neighbor peers.
    #[arg(long = "network.routing.max-neighbor-attempts")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_neighbor_attempts: Option<usize>,

    /// Max pending connections for neighbor bins.
    #[arg(long = "network.routing.max-neighbor-candidates")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_neighbor_candidates: Option<usize>,

    /// Max pending connections for balanced bins.
    #[arg(long = "network.routing.max-balanced-candidates")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_balanced_candidates: Option<usize>,
}

impl RoutingArgs {
    /// Build validated Kademlia routing configuration.
    pub fn routing_config(&self) -> KademliaConfig {
        let defaults = KademliaConfig::default();
        KademliaConfig {
            saturation_peers: self.saturation_peers.unwrap_or(defaults.saturation_peers),
            high_watermark: self.high_watermark.unwrap_or(defaults.high_watermark),
            client_reserved_slots: self
                .client_reserved_slots
                .unwrap_or(defaults.client_reserved_slots),
            low_watermark: self.low_watermark.unwrap_or(defaults.low_watermark),
            max_connect_attempts: self
                .max_connect_attempts
                .unwrap_or(defaults.max_connect_attempts),
            max_neighbor_attempts: self
                .max_neighbor_attempts
                .unwrap_or(defaults.max_neighbor_attempts),
            max_neighbor_candidates: self
                .max_neighbor_candidates
                .unwrap_or(defaults.max_neighbor_candidates),
            max_balanced_candidates: self
                .max_balanced_candidates
                .unwrap_or(defaults.max_balanced_candidates),
        }
    }
}
