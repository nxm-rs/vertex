//! Core Vertex client and SwarmNode for network participation.
//!
//! This crate provides:
//! - [`SwarmNode<N>`]: The main entry point for Swarm network participation
//! - [`SwarmClient`]: Implements [`SwarmReader`] for chunk retrieval
//! - [`ClientService`]: Background service for processing network events
//!
//! # Architecture
//!
//! ```text
//! SwarmNode<N: NodeTypes>
//! ├── swarm: Swarm<NodeBehaviour<N>>
//! ├── identity: Arc<SwarmIdentity>
//! ├── bootnode_connector: BootnodeConnector
//! └── client_event_tx / client_command_rx (for ClientService)
//!
//! SwarmClient
//! ├── accounting: A (AvailabilityAccounting)
//! ├── pricer: P (Pricer)
//! └── client_handle: ClientHandle (sends commands, receives responses)
//!
//! ClientService (runs in background)
//! ├── processes ClientEvent from network
//! ├── completes pending retrievals
//! └── handles business logic
//! ```
//!
//! # Usage
//!
//! ```ignore
//! use vertex_client_core::SwarmNode;
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
//! // Create SwarmClient for read operations
//! let client = SwarmClient::new(accounting, pricer, client_handle);
//!
//! // Run node event loop
//! node.run().await?;
//! ```

#![cfg_attr(not(feature = "std"), no_std)]

mod behaviour;
mod builder;
mod client;
mod node;
mod service;

pub use behaviour::{NodeBehaviour, NodeEvent};
pub use builder::{
    AccountingBuilder, BandwidthAccountingBuilder, BuiltSwarmComponents, DefaultComponentsBuilder,
    FixedPricerBuilder, KademliaTopologyBuilder, NoAccountingBuilder, PricerBuilder,
    SwarmBuilderContext, SwarmComponentsBuilder, TopologyBuilder,
};
pub use client::SwarmClient;
pub use node::{SwarmNode, SwarmNodeBuilder};
pub use service::{
    Cheque, ClientCommand, ClientEvent, ClientHandle, ClientService, RetrievalError,
    RetrievalResult,
};

// Re-export key types for convenience
pub use vertex_bandwidth_core::{
    Accounting, AccountingConfig, AccountingError, CreditAction, DEFAULT_BASE_PRICE, DebitAction,
    FixedPricer, MAX_PO, PeerState, Pricer,
};
pub use vertex_node_identity::SwarmIdentity;
