//! CLI arguments for Kademlia routing configuration.

use clap::Args;
use serde::{Deserialize, Serialize};

use super::{DepthAwareLimits, KademliaConfig};

/// Kademlia routing CLI arguments.
///
/// Pacing knobs (candidate budgets, evaluation cadence, dial rate) are not
/// exposed here: they are bundled per connection profile and selected via
/// `--network.connection-profile`.
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

        KademliaConfig { limits, ..defaults }
    }
}
