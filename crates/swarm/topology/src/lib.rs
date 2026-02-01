//! Swarm network topology management.
//!
//! Provides libp2p behaviour and handlers for Swarm peer discovery and connection
//! management. Operates at the libp2p layer using `PeerId` and `Multiaddr`.
//!
//! # Public API vs Internal Types
//!
//! This crate exposes two levels of command/event types:
//!
//! - **Public API** ([`TopologyCommand`], [`TopologyEvent`]): High-level commands and
//!   events for the node layer. Use these to interact with the topology behaviour.
//!
//! - **Internal** (`handler::Command`, `handler::Event`): Low-level per-connection
//!   messages between the behaviour and connection handlers. These are not exported.
//!
//! # Components
//!
//! - [`TopologyBehaviour`]: libp2p `NetworkBehaviour` managing handshake, hive, pingpong
//! - [`BootnodeConnector`]: Bootstrap node connection strategy
//! - [`dns`]: Resolution of `/dnsaddr/` multiaddrs

#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]

pub mod behaviour;
pub mod bootnode;
pub mod dns;
pub mod events;
pub mod handler;
pub mod protocol;

mod error;

pub use behaviour::TopologyBehaviour;
pub use bootnode::BootnodeConnector;
pub use dns::{DnsaddrResolveError, is_dnsaddr, resolve_all_dnsaddrs, resolve_dnsaddr};
pub use error::{TopologyError, TopologyResult};
pub use events::{TopologyCommand, TopologyEvent};
pub use handler::TopologyConfig;
pub use protocol::{
    TopologyInboundOutput, TopologyInboundUpgrade, TopologyOutboundInfo, TopologyOutboundOutput,
    TopologyOutboundRequest, TopologyOutboundUpgrade, TopologyUpgradeError,
};
