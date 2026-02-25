//! Swarm network topology management.
//!
//! Provides libp2p behaviour, handlers, and Kademlia routing for Swarm peer
//! discovery and connection management.

#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]

use libp2p::{PeerId, multiaddr::Protocol};

/// Extract PeerId from a multiaddr's /p2p/ component.
pub(crate) fn extract_peer_id(addr: &libp2p::Multiaddr) -> Option<PeerId> {
    addr.iter().find_map(|p| match p {
        Protocol::P2p(id) => Some(id),
        _ => None,
    })
}

pub mod behaviour;
pub mod events;
pub mod handle;
pub mod metrics;
pub mod nat_discovery;
pub mod kademlia;

mod composed;
mod error;
mod gossip;

pub use behaviour::{TopologyBehaviour, TopologyConfig, DEFAULT_DIAL_INTERVAL};
pub use vertex_net_dnsaddr::{is_dnsaddr, resolve_all};
pub use error::{
    DialError, DisconnectReason, RejectionReason, TopologyError, TopologyResult,
};
pub use events::{ConnectionDirection, TopologyCommand, TopologyEvent};
pub use metrics::TopologyMetrics;
pub use handle::{TopologyHandle, RoutingStats, BinStats};
pub use nat_discovery::LocalAddressManager;

// Re-export from peer registry crate
pub use vertex_swarm_peer_registry::DialReason;

pub use kademlia::{
    CandidateSelector, CandidateSnapshot, DepthAwareLimits, KademliaConfig,
    KademliaRouting, LimitsSnapshot, RoutingArgs, DEFAULT_NOMINAL, DEFAULT_TOTAL_TARGET,
};

pub use libp2p::Multiaddr;
