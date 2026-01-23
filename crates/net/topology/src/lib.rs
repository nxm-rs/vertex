//! Swarm network topology management.
//!
//! This crate provides the libp2p behaviour and protocol handlers for Swarm topology.
//! It handles peer discovery, bootnode connections, and topology events.
//!
//! # Components
//!
//! - **Behaviour**: libp2p `NetworkBehaviour` for topology management
//! - **Bootnode**: Initial network entry via bootstrap nodes
//! - **Manager**: Peer lifecycle management (connection state, disconnection)
//! - **Events**: Topology commands and events for communication with the swarm

#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]

pub mod behaviour;
pub mod bootnode;
pub mod events;
pub mod handler;
pub mod manager;
pub mod protocol;

mod error;

pub use behaviour::{Config as BehaviourConfig, PeerInfo, SwarmTopologyBehaviour};
pub use error::{TopologyError, TopologyResult};
pub use events::{TopologyCommand, TopologyEvent};
pub use protocol::{
    TopologyInboundOutput, TopologyInboundUpgrade, TopologyOutboundInfo, TopologyOutboundOutput,
    TopologyOutboundRequest, TopologyOutboundUpgrade, TopologyUpgradeError,
};

// Re-export key types
pub use bootnode::BootnodeConnector;
pub use manager::{PeerManager, PeerState};
