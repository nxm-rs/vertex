//! Swarm network topology management.
//!
//! This crate provides the libp2p behaviour and protocol handlers for Swarm topology.
//! It handles peer discovery, bootnode DNS resolution, and topology events.
//!
//! # Abstraction Boundary
//!
//! This crate operates at the libp2p layer and uses libp2p types (PeerId, Multiaddr,
//! ConnectionId). The client layer (`vertex-client-peermanager`) handles the
//! PeerId ↔ OverlayAddress mapping.
//!
//! # Components
//!
//! - **Behaviour**: libp2p `NetworkBehaviour` for topology management
//! - **Bootnode**: Bootstrap node connection management
//! - **DNS**: Resolution of `/dnsaddr/` multiaddrs for bootnodes
//! - **Events**: Topology commands and events using pure libp2p types

#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]

pub mod behaviour;
pub mod bootnode;
pub mod dns;
pub mod events;
pub mod handler;
pub mod protocol;

mod error;

pub use behaviour::{Config as BehaviourConfig, SwarmTopologyBehaviour};
pub use dns::{DnsaddrResolveError, is_dnsaddr, resolve_all_dnsaddrs, resolve_dnsaddr};
pub use error::{TopologyError, TopologyResult};
pub use events::{TopologyCommand, TopologyEvent};
pub use protocol::{
    TopologyInboundOutput, TopologyInboundUpgrade, TopologyOutboundInfo, TopologyOutboundOutput,
    TopologyOutboundRequest, TopologyOutboundUpgrade, TopologyUpgradeError,
};

// Re-export key types
pub use bootnode::BootnodeConnector;
