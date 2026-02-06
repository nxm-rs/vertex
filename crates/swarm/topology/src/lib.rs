//! Swarm network topology management.
//!
//! Provides libp2p behaviour, handlers, and Kademlia routing for Swarm peer
//! discovery and connection management.

#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]

// Public modules (user-facing)
pub mod behaviour;
pub mod dns;
pub mod events;
pub mod handle;
pub mod nat_discovery;
pub mod routing;
pub mod service;

// Internal modules (crate-only)
pub(crate) mod bootnode;
pub(crate) mod dial_tracker;
mod error;
mod gossip;
mod gossip_coordinator;
pub(crate) mod handler;
pub(crate) mod protocol;

// Public re-exports
pub use nat_discovery::{NatDiscovery, NatDiscoveryConfig};

pub use behaviour::TopologyBehaviour;
pub use gossip_coordinator::DepthProvider;
pub use dns::{DnsaddrResolveError, is_dnsaddr, resolve_all_dnsaddrs, resolve_dnsaddr};
pub use error::{TopologyError, TopologyResult};
pub use events::{TopologyCommand, TopologyServiceEvent};
pub use handle::TopologyHandle;
pub use handler::TopologyConfig;
pub use service::{CommandReceiver, TopologyBehaviourComponents, TopologyService, TopologyServiceConfig};

// Re-export DialTracker since it's exposed in TopologyBehaviourComponents
pub use dial_tracker::DialTracker;

// Re-export routing types at crate root for convenience
pub use routing::{
    KademliaConfig, KademliaRouting, PeerFailureProvider, RoutingStats,
    DEFAULT_CLIENT_RESERVED_SLOTS, DEFAULT_HIGH_WATERMARK, DEFAULT_LOW_WATERMARK,
    DEFAULT_MANAGE_INTERVAL, DEFAULT_MAX_BALANCED_CANDIDATES, DEFAULT_MAX_CONNECT_ATTEMPTS,
    DEFAULT_MAX_NEIGHBOR_ATTEMPTS, DEFAULT_MAX_NEIGHBOR_CANDIDATES, DEFAULT_SATURATION_PEERS,
    MAX_PO,
};

// Re-export libp2p types used in public API
pub use libp2p::Multiaddr;
