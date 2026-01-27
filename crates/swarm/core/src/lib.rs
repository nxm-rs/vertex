//! Core Swarm node orchestration with libp2p integration.
//!
//! This crate provides the central coordination layer for Swarm nodes:
//! - [`SwarmNode<N>`]: The main entry point for Swarm network participation
//! - [`NodeBehaviour`]: The composed libp2p behaviour
//! - [`ClientService`]: Background service for processing network events
//!
//! # Architecture
//!
//! ```text
//! SwarmNode<N: NodeTypes>
//! ├── swarm: Swarm<NodeBehaviour<N>>
//! ├── identity: Arc<SwarmIdentity>
//! ├── peer_manager: Arc<PeerManager>
//! ├── kademlia: Arc<KademliaTopology>
//! └── bootnode_connector: BootnodeConnector
//!
//! ClientService (runs in background)
//! ├── processes ClientEvent from network
//! ├── completes pending retrievals
//! └── handles business logic
//! ```
//!
//! # Abstraction Boundary
//!
//! The SwarmNode serves as the bridge between:
//! - **libp2p layer**: Uses PeerId, Multiaddr, ConnectionId
//! - **Swarm layer**: Uses OverlayAddress
//!
//! # Usage
//!
//! ```ignore
//! use vertex_swarm_core::{SwarmNode, ClientService, ClientHandle};
//! use vertex_swarm_api::NetworkConfig;
//!
//! // Build node with network config
//! let (mut node, client_service, client_handle) = SwarmNode::<MyNodeTypes>::builder(identity)
//!     .with_network_config(&my_network_args)
//!     .build()
//!     .await?;
//!
//! // Spawn client service
//! tokio::spawn(client_service.run());
//!
//! // Run node event loop
//! node.run().await?;
//! ```

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(feature = "cli")]
pub mod args;
#[cfg(feature = "cli")]
mod config;
#[cfg(feature = "cli")]
mod constants;

mod behaviour;
mod bootnodes;
mod node;
mod service;
mod stats;

pub use behaviour::{NodeEvent, SwarmNodeBehaviour};
pub use bootnodes::BootnodeProvider;
#[cfg(feature = "cli")]
pub use config::SwarmConfig;
pub use node::{SwarmNode, SwarmNodeBuilder, SwarmNodeType};
pub use service::{
    Cheque, ClientCommand, ClientEvent, ClientHandle, ClientService, RetrievalError,
    RetrievalResult,
};
pub use stats::{StatsConfig, spawn_stats_task};
