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
mod client;
mod node;
pub mod protocol;
mod service;
mod stats;

pub use node::{
    BaseNode, BootNode, BootNodeBuilder, BootnodeBehaviour, BootnodeEvent, BuilderConfig,
    BuiltInfrastructure, ClientNode, ClientNodeBehaviour, ClientNodeBuilder, ClientNodeEvent,
    StorerNode, StorerNodeBuilder,
};

pub use vertex_swarm_api::SwarmNodeType;

pub use service::{
    ClientCommand, ClientEvent, ClientHandle, ClientService, RetrievalError, RetrievalResult,
};

pub use protocol::{PseudosettleEvent, SwapEvent};

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
