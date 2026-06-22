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
mod staggered_race;
mod throttle;

pub use node::{
    BaseNode, BuiltInfrastructure, ClientCore, ClientCoreCtx, ClientLauncher, ClientNode,
    ClientNodeBuilder, LaunchedClient, NodeBuildError, PseudosettleWiring, SharedAccounting,
    assemble_client_core, spawn_client_command_bridge,
};
#[cfg(not(target_arch = "wasm32"))]
pub use node::{BootNode, BootNodeBuilder, StorerNode, StorerNodeBuilder, StorerPullsyncControl};

pub use vertex_swarm_api::SwarmNodeType;

pub use client_service::{ChunkTransferError, ClientHandle, ClientService, RetrievalResult};
#[cfg(feature = "swap")]
pub use protocol::SwapEvent;
pub use protocol::{
    ClientCommand, ClientEvent, FailureKind, PseudosettleEvent, PushResponseTx, RetrievalResponseTx,
};

pub use selection::{AccountingSettlement, PeerScores, PeerSelector, SettlementTrigger};
pub use staggered_race::{RETRIEVAL_STAGGER, RaceFailure, race_candidates};
pub use throttle::SelfThrottle;

pub use bootnodes::BootnodeProvider;
pub use node::stats::StatsConfig;
pub use node::task::spawn_stats_task;
