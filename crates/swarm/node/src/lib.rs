//! Swarm node implementation with libp2p networking.
//!
//! Provides [`BootNode`], [`ClientNode`], and [`StorerNode`] for different
//! levels of network participation.

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(feature = "cli")]
pub mod args;
#[cfg(feature = "cli")]
mod config;

#[cfg(feature = "cli")]
pub use config::ProtocolConfig;

mod bootnodes;
mod client_service;
mod node;
mod protocol;
mod swarm_client;

pub use node::{
    BaseNode, BootNode, BootNodeBuilder, BootnodeBehaviour, BootnodeEvent, BuiltInfrastructure,
    ClientNode, ClientNodeBehaviour, ClientNodeBuilder, ClientNodeEvent, NodeBuildError,
    StorerNode, StorerNodeBuilder,
};

pub use vertex_swarm_api::SwarmNodeType;

pub use protocol::{ClientCommand, ClientEvent};
pub use client_service::{ClientHandle, ClientService, RetrievalError, RetrievalResult};

pub use protocol::PseudosettleEvent;

pub use protocol::{
    BehaviourConfig as ClientBehaviourConfig, ClientBehaviour, ClientHandler,
    HandlerConfig as ClientHandlerConfig,
};

pub use swarm_client::{BootnodeClient, Client, FullClient};

pub use bootnodes::BootnodeProvider;
pub use node::stats::{StatsConfig, spawn_stats_task};

