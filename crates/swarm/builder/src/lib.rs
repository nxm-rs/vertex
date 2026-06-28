//! Swarm node builder infrastructure.
//!
//! Provides layered builders for constructing Swarm nodes:
//! - [`NodeBuilder`] / [`DefaultNodeBuilder`] - Bootnode builder
//! - [`ClientNodeBuilder`] / [`DefaultClientBuilder`] - Client node builder
//! - [`StorerNodeBuilder`] / [`DefaultStorerBuilder`] - Storer node builder
//!
//! Build returns [`BuiltNode`] which contains the task and RPC providers.
//!
//! # Build modes
//!
//! The crate has two build modes, selected by the `chain` cargo feature:
//!
//! - Default (`chain` off): the lean, chain-free build. No Ethereum RPC stack
//!   is compiled in; only chain-free node types (a bootnode, a client without
//!   SWAP) launch, and a chain-needing node type hard-fails the build with
//!   `SwarmNodeError::ChainRequired`. This is what the default `vertex` binary
//!   and the wasm client resolve, and the cone guard enforces that the chain
//!   crates and their alloy RPC dependencies never reach this cone.
//! - `chain` on: pulls `vertex-chain` and the on-chain chequebook client, and
//!   constructs a shared Ethereum alloy provider for chain-needing node types.
//!   The chain is a shared provider, not a long-lived service: the launch path
//!   consults `SwarmNodeType::needs_chain` and a configured RPC URL, builds a
//!   wallet-filled provider signed by the node identity, validates the connected
//!   chain id at startup, and hands back a cloneable handle that future chain
//!   consumers (the SWAP settlement service) borrow. There is no background
//!   chain task to spawn.
//!
//! The chain knobs (RPC URL and transaction tuning) live on the node configs in
//! every build; without the feature they are inert.
//!
//! Native-only: it depends unconditionally on the redb backend, so it never
//! enters the wasm cone (the wasm client composes through `vertex-swarm-node`).
//! The store seams therefore carry `RedbDatabase`-typed factory closures without
//! cfg-gating.

#[cfg(feature = "chain")]
mod chain;
pub mod config;
mod error;
mod handle;
mod launch;
mod node;
mod protocol;
#[cfg(feature = "reserve")]
mod storer;

// Builders
pub use node::{ClientNodeBuilder, DefaultClientBuilder, DefaultNodeBuilder, NodeBuilder};

// The Swarm `NodeProtocol` the node builder launches. Lives here so its serve
// view can name the gRPC adapter.
pub use protocol::SwarmProtocol;

// Build outputs
pub use handle::{BuiltBootnode, BuiltClient, BuiltNode, BuiltStorer};

// Chunk providers: the network retrieval/push provider and its config-gated
// download-verification wrapper live in `vertex-swarm-node` (the wasm-clean cone
// both client entry points share); re-exported here so the existing builder
// import paths stay stable.
pub use vertex_swarm_node::{ChunkVerifyConfig, NetworkChunkProvider, VerifyingChunkProvider};

// Configs
pub use config::{BootnodeConfig, ClientConfig};

// Launch types (for SwarmLaunchConfig associated types)
pub use launch::{BootnodeLaunchTypes, ClientLaunchTypes};

// The storer cone (builder and config) behind the `reserve` feature; its launch
// types are the shared `ClientLaunchTypes`.
#[cfg(feature = "reserve")]
pub use storer::{DefaultStorerBuilder, StorerConfig, StorerNodeBuilder};

// Errors
pub use error::SwarmNodeError;

// Chain provider seam (behind the `chain` feature)
#[cfg(feature = "chain")]
pub use chain::{SharedChainProvider, build_chain_provider};

// Re-exports
pub use vertex_swarm_accounting::{AccountingBuilder, NoAccountingBuilder};
pub use vertex_swarm_api::{BootnodeComponents, ClientComponents, StorerComponents};
