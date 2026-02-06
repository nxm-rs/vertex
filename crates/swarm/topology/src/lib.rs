//! Swarm network topology management.
//!
//! Provides libp2p behaviour, handlers, and Kademlia routing for Swarm peer
//! discovery and connection management.

#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]

pub mod behaviour;
mod builder;
pub mod dns;
pub mod events;
pub mod handle;
pub mod nat_discovery;
pub mod routing;

pub(crate) mod bootnode;
pub(crate) mod dial_tracker;
mod error;
mod gossip;
mod gossip_coordinator;
pub(crate) mod handler;
pub(crate) mod protocol;

pub use behaviour::{TopologyBehaviour, TopologyBehaviourConfig, DEFAULT_DIAL_INTERVAL};
pub use builder::SwarmTopologyBuilder;
pub use dns::{DnsaddrResolveError, is_dnsaddr, resolve_all_dnsaddrs, resolve_dnsaddr};
pub use error::{TopologyError, TopologyResult};
pub use events::{TopologyCommand, TopologyServiceEvent};
pub use gossip_coordinator::DepthProvider;
pub use handle::TopologyHandle;
pub use handler::TopologyConfig;
pub use nat_discovery::{NatDiscovery, NatDiscoveryConfig};

pub use routing::{
    KademliaConfig, KademliaRouting, PeerFailureProvider, RoutingStats, SwarmRouting,
    DEFAULT_CLIENT_RESERVED_SLOTS, DEFAULT_HIGH_WATERMARK, DEFAULT_LOW_WATERMARK,
    DEFAULT_MANAGE_INTERVAL, DEFAULT_MAX_BALANCED_CANDIDATES, DEFAULT_MAX_CONNECT_ATTEMPTS,
    DEFAULT_MAX_NEIGHBOR_ATTEMPTS, DEFAULT_MAX_NEIGHBOR_CANDIDATES, DEFAULT_SATURATION_PEERS,
    MAX_PO,
};

pub use libp2p::Multiaddr;
