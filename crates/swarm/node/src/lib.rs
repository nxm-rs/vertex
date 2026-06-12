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
mod selection;
mod swarm_client;

pub use node::{
    BaseNode, BuiltInfrastructure, ClientLauncher, ClientNode, ClientNodeBuilder, LaunchedClient,
    NodeBuildError,
};
#[cfg(not(target_arch = "wasm32"))]
pub use node::{BootNode, BootNodeBuilder, StorerNode, StorerNodeBuilder};

pub use vertex_swarm_api::SwarmNodeType;

pub use client_service::{ClientHandle, ClientService, RetrievalError, RetrievalResult};
#[cfg(feature = "swap")]
pub use protocol::SwapEvent;
pub use protocol::{ClientCommand, ClientEvent, PseudosettleEvent};

pub use selection::{AccountingSettlement, PeerScores, PeerSelector, SettlementTrigger};
pub use swarm_client::{BootnodeClient, Client, FullClient};

pub use bootnodes::BootnodeProvider;
pub use node::stats::StatsConfig;
pub use node::task::spawn_stats_task;
