//! Core Vertex client with libp2p integration.
//!
//! This crate is the libp2p boundary layer for the Swarm protocol.
//!
//! With the `cli` feature enabled, also provides [`ProtocolArgs`] and [`ProtocolConfig`]
//! for CLI argument parsing and protocol configuration.

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(feature = "cli")]
pub mod args;
#[cfg(feature = "cli")]
mod config;

#[cfg(feature = "cli")]
pub use config::ProtocolConfig;

mod bootnodes;
mod client;
mod node;
pub mod protocol;
mod service;
mod stats;

pub use node::behaviour::{NodeBehaviour, NodeEvent};
pub use node::bootnode::{BootNode, BootNodeBuilder, BootnodeBehaviour, BootnodeEvent};
pub use node::{SwarmNode, SwarmNodeBuilder};

// Re-export SwarmNodeType from vertex-swarm-api
pub use vertex_swarm_api::SwarmNodeType;

pub use service::{
    ClientCommand, ClientEvent, ClientHandle, ClientService, RetrievalError, RetrievalResult,
};

// Re-export settlement event types for wiring
pub use protocol::{PseudosettleEvent, SwapEvent};

// Re-export protocol behaviour types
pub use protocol::{
    BehaviourConfig as ClientBehaviourConfig, ClientBehaviour, ClientHandler,
    HandlerConfig as ClientHandlerConfig,
};

pub use client::{BootnodeClient, BuiltSwarmComponents, Client, FullClient};

pub use bootnodes::BootnodeProvider;
pub use stats::{StatsConfig, spawn_stats_task};

pub use vertex_swarm_bandwidth::{
    Accounting, AccountingError, AccountingPeerHandle, ClientAccounting, FixedPricer, PeerState,
    Pricer, ProvideAction, ReceiveAction,
};
