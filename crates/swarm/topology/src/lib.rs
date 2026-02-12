//! Swarm network topology management.
//!
//! Provides libp2p behaviour, handlers, and Kademlia routing for Swarm peer
//! discovery and connection management.

#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]

pub mod behaviour;
pub mod dns;
pub mod events;
pub mod handle;
pub mod metrics;
pub mod nat_discovery;
pub mod routing;

mod error;
mod gossip;
pub(crate) mod handler;
pub(crate) mod protocol;

pub use behaviour::{TopologyBehaviour, TopologyConfig, DEFAULT_DIAL_INTERVAL};
pub use dns::{DnsaddrResolveError, is_dnsaddr, resolve_all_dnsaddrs, resolve_dnsaddr};
pub use error::{TopologyError, TopologyResult};
pub use events::{
    ConnectionDirection, DisconnectReason, RejectionReason, TopologyCommand, TopologyEvent,
};
pub use metrics::{TopologyMetrics, record_event as record_topology_event};
pub use handle::TopologyHandle;
pub use nat_discovery::{NatDiscovery, NatDiscoveryConfig};

// Re-export from peer registry crate
pub use vertex_swarm_peer_registry::DialReason;

pub use routing::{KademliaConfig, KademliaRouting, RoutingArgs, SwarmRouting};

pub use libp2p::Multiaddr;
