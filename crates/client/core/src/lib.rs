//! Core Vertex client with libp2p integration.
//!
//! This crate is **THE LIBP2P BOUNDARY**:
//! - **Below**: Uses PeerId, Multiaddr, libp2p types
//! - **Above**: Exposes only OverlayAddress and NetworkEvent/NetworkCommand
//!
//! # Architecture
//!
//! ```text
//! vertex-swarm-core (business logic - libp2p FREE)
//!         │
//!         ▼
//! THIS CRATE: vertex-client-core (THE BOUNDARY)
//! - SwarmNode: wraps libp2p::Swarm
//! - NodeBehaviour: composed NetworkBehaviour
//! - Client: implements SwarmClient trait
//! - ClientService: event processing
//! - PeerId ↔ OverlayAddress translation via PeerManager
//!         │
//!         ▼
//! vertex-net-* (libp2p protocol implementations)
//! - vertex-net-topology: handshake, hive, pingpong
//! - vertex-net-pricing, vertex-net-retrieval, vertex-net-pushsync
//! ```
//!
//! # Components
//!
//! - [`SwarmNode`]: Wraps libp2p::Swarm, coordinates network activity
//! - [`NodeBehaviour`]: Composed libp2p NetworkBehaviour
//! - [`Client`]: Unified client implementing [`SwarmClient`] trait
//! - [`ClientService`]: Processes network events
//! - [`ClientHandle`]: Sends commands to the network layer
//! - [`BootnodeProvider`]: Bootstrap node address resolution

#![cfg_attr(not(feature = "std"), no_std)]

mod bootnodes;
mod client;
mod node;
pub mod protocol;
mod service;
mod stats;

pub use node::{SwarmNode, SwarmNodeBuilder};
pub use node::behaviour::{NodeEvent, SwarmNodeBehaviour};
pub use node::bootnode::{BootNode, BootNodeBuilder, BootnodeBehaviour, BootnodeEvent};

// Re-export SwarmNodeType from vertex-swarm-api
pub use vertex_swarm_api::SwarmNodeType;

pub use service::{
    ClientCommand, ClientEvent, ClientHandle, ClientService,
    RetrievalError, RetrievalResult,
};

// Re-export settlement event types for wiring
pub use protocol::{PseudosettleEvent, SwapEvent};

// Re-export protocol behaviour types
pub use protocol::{
    BehaviourConfig as ClientBehaviourConfig, SwarmClientBehaviour,
    HandlerConfig as ClientHandlerConfig, SwarmClientHandler,
};

pub use client::{BootnodeClient, BuiltSwarmComponents, Client, FullClient};

pub use bootnodes::BootnodeProvider;
pub use stats::{StatsConfig, spawn_stats_task};

pub use vertex_bandwidth_core::{
    Accounting, AccountingError, AccountingPeerHandle, CreditAction, DebitAction, FixedPricer,
    PeerState, Pricer,
};
