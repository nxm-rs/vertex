//! Swarm network topology management.
//!
//! Provides libp2p behaviour, handlers, and Kademlia routing for Swarm peer
//! discovery and connection management.

#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]

pub(crate) use vertex_net_utils::extract_peer_id;

mod behaviour;
mod connection_handlers;
mod dialing;
mod protocol_handlers;
mod events;
mod handle;
pub mod metrics;
mod nat_discovery;
mod kademlia;

mod composed;
mod error;
mod gossip;

#[cfg(test)]
pub(crate) mod test_support;

pub use behaviour::{TopologyBehaviour, TopologyConfig, DEFAULT_DIAL_INTERVAL};
pub use error::{
    DialError, DisconnectReason, RejectionReason, TopologyError, TopologyResult,
};
pub use events::{ConnectionDirection, DialReason, TopologyCommand, TopologyEvent};
pub use handle::{TopologyHandle, RoutingStats, BinStats};

pub use kademlia::{KademliaConfig, RoutingArgs};

pub use libp2p::Multiaddr;
