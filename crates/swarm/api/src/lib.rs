//! Swarm API - core abstractions for Ethereum Swarm.
//!
//! This crate defines *what* the Swarm protocol does without prescribing *how*.
//! All traits are libp2p-agnostic; the libp2p boundary lives in `vertex-swarm-node`.
//!
//! # Type Hierarchy
//!
//! Node capabilities are modelled as a trait chain where each level adds
//! associated types for additional services:
//!
//! - [`SwarmPrimitives`] - `Spec` + `Identity` (pure data, no services)
//! - [`SwarmNetworkTypes`] - adds `Topology` (peer discovery)
//! - [`SwarmClientTypes`] - adds `Accounting` (bandwidth + pricing)
//! - [`SwarmStorerTypes`] - adds `Store` (local chunk persistence)
//!
//! # Component Containers
//!
//! Each capability level has a runtime container:
//!
//! - [`BootnodeComponents`] - topology only
//! - [`ClientComponents`] - topology + accounting
//! - [`StorerComponents`] - topology + accounting + store
//!
//! Access is abstracted via [`HasTopology`], [`HasAccounting`], [`HasStore`],
//! and [`HasIdentity`] traits.
//!
//! # Protocol Integration
//!
//! [`SwarmProtocol`] implements [`vertex_node_api::NodeProtocol`], bridging the
//! Swarm domain with the generic node infrastructure.
//!
//! # Peer Reporting
//!
//! [`PeerReporter`] is the single sanctioned path for subsystems to affect a
//! peer's score, with [`SwarmScoringEvent`] as the shared event vocabulary.
//! [`PeerAffordability`] lets protocol handlers ask bandwidth accounting
//! whether a peer can pay for a request, and [`PeerLifecycleEvent`] carries
//! the resulting lifecycle decisions (warnings, disconnects, bans) to
//! subscribers such as topology.

#![warn(missing_docs)]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

mod components;
mod config;
mod error;
mod identity;
mod protocol;
mod providers;
mod reporting;
mod rpc;
mod spec;
mod swarm;
mod types;

pub use self::components::{
    BandwidthMode, BootnodeComponents, ClientComponents, Direction, HasAccounting, HasIdentity,
    HasStore, HasTopology, StorerComponents, SwarmAccountingConfig, SwarmBandwidthAccounting,
    SwarmClientAccounting, SwarmLocalStore, SwarmLocalStoreConfig, SwarmPeerBandwidth,
    SwarmPeerResolver, SwarmPeerState, SwarmPricing, SwarmPricingBuilder, SwarmPricingConfig,
    SwarmSettlementProvider, SwarmTopology, SwarmTopologyBins, SwarmTopologyCommands,
    SwarmTopologyPeers, SwarmTopologyRouting, SwarmTopologyState, SwarmTopologyStats,
};
pub use self::config::{
    DEFAULT_PEER_BAN_THRESHOLD, DEFAULT_PEER_DISCONNECT_THRESHOLD, DEFAULT_PEER_MAX_PER_BIN,
    DEFAULT_PEER_WARN_THRESHOLD, DefaultPeerConfig, DefaultStorageConfig, METADATA_OVERHEAD_FACTOR,
    NodeTask, NodeTaskFn, PeerConfigValues, SwarmBootnodeConfig, SwarmClientConfig,
    SwarmClientLaunchConfig, SwarmIdentityConfig, SwarmLaunchConfig, SwarmNetworkConfig,
    SwarmPeerConfig, SwarmRoutingConfig, SwarmStorageConfig, SwarmStorerConfig,
    SwarmStorerLaunchConfig, estimate_chunks_for_bytes, estimate_storage_bytes,
};
pub use self::error::{
    ConfigAddressKind, ConfigError, ConfigResult, IdentityError, SwarmError, SwarmResult,
};
pub use self::identity::SwarmIdentity;
pub use self::protocol::SwarmProtocol;
pub use self::providers::{
    ChunkRetrievalResult, PushReceipt, SwarmChunkProvider, SwarmChunkSender,
};
pub use self::reporting::{
    BanCause, DisconnectCause, PeerAffordability, PeerLifecycleEvent, PeerReporter, ReportSource,
    SwarmScoringEvent,
};
pub use self::rpc::RpcProviders;
pub use self::spec::{
    StaticSwarmSpecProvider, SwarmSpec, SwarmSpecParser, SwarmSpecProvider, SwarmToken,
};
pub use self::swarm::{SwarmClient, SwarmStorer};
pub use self::types::{
    AccountingOf, BandwidthOf, IdentityOf, PricingOf, SpecOf, StoreOf, SwarmClientTypes,
    SwarmNetworkTypes, SwarmNodeType, SwarmPrimitives, SwarmStorerTypes, TopologyOf,
};

// Re-export primitives for convenience
pub use nectar_primitives::{
    AnyChunk, Chunk, ChunkAddress, ChunkType, ChunkTypeId, ChunkTypeSet, ContentChunk,
    SingleOwnerChunk, StandardChunkSet,
};
pub use vertex_swarm_primitives::{
    OverlayAddress, Stamp, StampedChunk, StorageRadius, ValidatedChunk, ValidationError,
};

// Re-export libp2p types used in config traits
pub use libp2p::Multiaddr;
