//! Core Vertex client for Swarm network participation.
//!
//! This crate provides [`SwarmClient`], a unified client for all node types:
//!
//! - **Bootnode**: Topology only (peer discovery)
//! - **Light node**: + Accounting, implements [`SwarmReader`]
//! - **Publisher node**: + [`SwarmWriter`]
//!
//! For the SwarmNode and network orchestration, see `vertex_swarm_core`.
//!
//! # Client Structure
//!
//! ```text
//! SwarmClient<Types, A = (), P = ()>
//! ├── topology: Types::Topology        (always present)
//! ├── accounting: Option<Arc<A>>       (None for bootnodes)
//! ├── pricer: Option<Arc<P>>           (None for bootnodes)
//! └── client_handle: ClientHandle
//! ```
//!
//! # Usage
//!
//! ```ignore
//! use vertex_client_core::SwarmClient;
//! use vertex_swarm_core::ClientHandle;
//!
//! // Bootnode (peer discovery only)
//! let bootnode = SwarmClient::<MyBootnodeTypes>::bootnode(topology, handle);
//!
//! // Light node (can retrieve chunks)
//! let client = SwarmClient::new(topology, accounting, pricer, handle);
//! let chunk = client.get(&address).await?;
//!
//! // Publisher node (can also upload)
//! client.put(chunk, &storage_proof).await?;
//! ```

#![cfg_attr(not(feature = "std"), no_std)]

mod client;

pub use client::{BootnodeClient, LightClient, PublisherClient, SwarmClient};

// Re-export from swarm-core for convenience
pub use vertex_swarm_core::{
    Cheque, ClientCommand, ClientEvent, ClientHandle, ClientService, NodeBehaviour, NodeEvent,
    RetrievalError, RetrievalResult, SwarmNode, SwarmNodeBuilder,
};

// Re-export builder infrastructure from swarm-builder
pub use vertex_swarm_builder::{
    AccountingBuilder, BandwidthAccountingBuilder, BuiltSwarmComponents, DefaultComponentsBuilder,
    FixedPricerBuilder, KademliaTopologyBuilder, NoAccountingBuilder, PricerBuilder,
    SwarmBuilderContext, SwarmComponentsBuilder, TopologyBuilder,
};

// Re-export key types for convenience
pub use vertex_bandwidth_core::{
    Accounting, AccountingConfig, AccountingError, CreditAction, DEFAULT_BASE_PRICE, DebitAction,
    FixedPricer, MAX_PO, PeerState, Pricer,
};
