//! Swarm API - Core abstractions for Ethereum Swarm.

#![warn(missing_docs)]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

mod components;
mod config;
mod error;
mod identity;
mod protocol;
mod providers;
mod rpc;
mod spec;
mod swarm;
mod types;

pub use self::components::{
    BandwidthMode, BootnodeComponents, ClientComponents, Direction, HasAccounting, HasIdentity,
    HasStore, HasTopology, StorerComponents, SwarmAccountingConfig, SwarmBandwidthAccounting,
    SwarmClientAccounting, SwarmLocalStore, SwarmLocalStoreConfig, SwarmPeerBandwidth,
    SwarmPeerResolver, SwarmPeerState, SwarmPricing, SwarmPricingBuilder, SwarmPricingConfig,
    SwarmScoreStore, SwarmSettlementProvider, SwarmTopology, SwarmTopologyBins,
    SwarmTopologyCommands, SwarmTopologyPeers, SwarmTopologyRouting, SwarmTopologyState,
    SwarmTopologyStats,
};
pub use self::config::{
    DefaultPeerConfig, DefaultStorageConfig, METADATA_OVERHEAD_FACTOR,
    NodeTask, NodeTaskFn, PeerConfigValues, SwarmBootnodeConfig, SwarmClientConfig,
    SwarmClientLaunchConfig, SwarmIdentityConfig, SwarmLaunchConfig, SwarmNetworkConfig,
    SwarmPeerConfig, SwarmRoutingConfig, SwarmStorageConfig, SwarmStorerConfig,
    SwarmStorerLaunchConfig, DEFAULT_PEER_BAN_THRESHOLD, DEFAULT_PEER_MAX_PER_BIN,
    DEFAULT_PEER_WARN_THRESHOLD, estimate_chunks_for_bytes, estimate_storage_bytes,
};
pub use self::error::{ConfigAddressKind, ConfigError, ConfigResult, SwarmError, SwarmResult};
pub use self::identity::SwarmIdentity;
pub use self::protocol::SwarmProtocol;
pub use self::providers::{
    ChunkRetrievalResult, ChunkSendReceipt, SwarmChunkProvider, SwarmChunkSender,
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
pub use vertex_swarm_primitives::{OverlayAddress, ValidatedChunk, ValidationError};

// Re-export libp2p types used in config traits
pub use libp2p::Multiaddr;
