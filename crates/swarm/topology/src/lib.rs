//! Swarm network topology management.
//!
//! Provides libp2p behaviour, handlers, and Kademlia routing for Swarm peer
//! discovery and connection management.

#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]

pub(crate) use vertex_net_utils::extract_peer_id;

mod behaviour;
mod connection_handlers;
mod dialing;
mod events;
mod handle;
mod kademlia;
pub mod metrics;
mod nat_discovery;
mod protocol_handlers;

mod composed;
mod error;
mod gossip;

#[cfg(test)]
pub(crate) mod test_support;

pub use behaviour::{DEFAULT_DIAL_INTERVAL, TopologyBehaviour, TopologyConfig};
pub use error::{DialError, DisconnectReason, RejectionReason, TopologyError, TopologyResult};
pub use events::{ConnectionDirection, DialReason, TopologyCommand, TopologyEvent};
pub use handle::{BinStats, RoutingStats, TopologyHandle};

pub use kademlia::{KademliaConfig, RoutingArgs};
