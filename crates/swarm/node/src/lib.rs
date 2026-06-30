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
mod chunks;
mod client_service;
mod inflight;
mod node;
mod protocol;
mod retrieval_latency;
mod selection;
mod staggered_race;

pub use node::{
    BaseNode, BuiltInfrastructure, ClientCore, ClientCoreCtx, ClientLauncher, ClientNode,
    ClientNodeBuilder, ClientNodeParts, ClientTailParams, LaunchedClient, NodeBuildError,
    NodeRunParts, NodeRunTaskFn, PseudosettleWiring, RunTaskFn, SettlementEventSenders,
    SharedAccounting, VerifiedChunkProvider, assemble_client_core, build_client_core_tail,
    single_task, spawn_client_command_bridge,
};
#[cfg(not(target_arch = "wasm32"))]
pub use node::{BootNode, BootNodeBuilder};
#[cfg(feature = "swap")]
pub use node::{
    ClientSwapParams, LauncherSwapConfig, NodeChainError, SwapWiring, node_chain_provider,
};
#[cfg(all(not(target_arch = "wasm32"), feature = "storer"))]
pub use node::{StorerNode, StorerNodeBuilder, StorerPullsyncControl};
/// The shared chain provider handle, re-exported so client entry points and the
/// builder consume one path. Available whenever SWAP (which requires the chain)
/// is enabled.
#[cfg(feature = "swap")]
pub use vertex_chain::SharedChainProvider;

pub use vertex_swarm_api::SwarmNodeType;

pub use client_service::{ChunkTransferError, ClientHandle, ClientService, RetrievalResult};
#[cfg(feature = "swap")]
pub use protocol::SwapEvent;
pub use protocol::{
    ClientCommand, ClientEvent, FailureKind, PseudosettleEvent, PushResponseTx, RetrievalResponseTx,
};

pub use inflight::{DEFAULT_PEER_INFLIGHT_CAP, PeerInflightLimiter};
pub use selection::{AccountingSettlement, PeerScores, PeerSelector, SettlementTrigger};
pub use staggered_race::{RETRIEVAL_STAGGER, RaceFailure, race_candidates, race_walk};

pub use bootnodes::BootnodeProvider;
pub use chunks::{ChunkVerifyConfig, NetworkChunkProvider, VerifyingChunkProvider};
pub use node::stats::StatsConfig;
pub use node::task::spawn_stats_task;
