//! CLI arguments for Kademlia routing configuration.

use clap::Args;
use serde::{Deserialize, Serialize};

use super::{DepthAwareLimits, KademliaConfig};

/// Kademlia routing CLI arguments.
#[derive(Debug, Args, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RoutingArgs {
    /// Total target connected peers across all bins.
    #[arg(long = "network.routing.total-target")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_target: Option<usize>,

    /// Minimum peers per bin for depth calculation.
    #[arg(long = "network.routing.nominal")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nominal: Option<usize>,

    /// Extra headroom for accepting inbound connections above target.
    #[arg(long = "network.routing.inbound-headroom")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inbound_headroom: Option<usize>,

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

        let total_target = self.total_target.unwrap_or(defaults.limits.total_target());
        let nominal = self.nominal.unwrap_or(defaults.limits.nominal());

        let mut limits = DepthAwareLimits::new(total_target, nominal);
        if let Some(headroom) = self.inbound_headroom {
            limits = limits.with_inbound_headroom(headroom);
        }

        KademliaConfig {
            limits,
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
