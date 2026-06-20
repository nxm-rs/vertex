//! Swarm API - libp2p-agnostic trait surface for an Ethereum Swarm node. The
//! libp2p boundary lives in `vertex-swarm-node`.
//!
//! Node capabilities form a trait chain, each level adding associated types:
//! [`SwarmPrimitives`] (`Spec` + `Identity`), [`SwarmNetworkTypes`] (+ topology),
//! [`SwarmClientTypes`] (+ accounting), [`SwarmStorerTypes`] (+ store).
//!
//! Runtime containers mirror the chain: [`BootnodeComponents`] (topology),
//! [`ClientComponents`] (+ chunk client), [`StorerComponents`] (+ store), accessed
//! through [`HasTopology`], [`HasChunkClient`], [`HasStore`], [`HasIdentity`].
//! Accounting is not a component: it is wired into the network chunk client and
//! shared through an `Arc` at launch; bootnodes run a listen-only pricing handler.
//!
//! [`SwarmProtocol`] implements [`vertex_node_api::NodeProtocol`].
//!
//! [`PeerReporter`] is the only path for subsystems to affect a peer's score
//! ([`SwarmScoringEvent`] is the event vocabulary). [`PeerAffordability`] asks
//! accounting whether a peer can pay; [`PeerLifecycleEvent`] carries the resulting
//! decisions (warnings, disconnects, bans) to subscribers such as topology.

#![warn(missing_docs)]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

mod accounting;
mod components;
mod config;
mod error;
mod identity;
mod protocol;
mod providers;
mod reporting;
mod spec;
mod swarm;
mod types;

pub use self::accounting::{Au, AuConversionError};
pub use self::components::{
    AccountingAction, BandwidthMode, BinCursorStore, BinScanItem, BootnodeComponents,
    ClientComponents, Direction, HasChunkClient, HasIdentity, HasReserve, HasStore, HasTopology,
    ReserveStore, SettableRadius, StorerComponents, SwarmAccountingConfig,
    SwarmBandwidthAccounting, SwarmClientAccounting, SwarmLocalStore, SwarmLocalStoreConfig,
    SwarmPeerBandwidth, SwarmPeerResolver, SwarmPeerState, SwarmPricing, SwarmPricingBuilder,
    SwarmPricingConfig, SwarmSettlementProvider, SwarmTopology, SwarmTopologyBins,
    SwarmTopologyCommands, SwarmTopologyPeers, SwarmTopologyReporting, SwarmTopologyRouting,
    SwarmTopologyState, SwarmTopologyStats, construct,
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
pub use self::spec::{
    DEFAULT_SATURATION_PEERS, StaticSwarmSpecProvider, SwarmSpec, SwarmSpecParser,
    SwarmSpecProvider, SwarmToken,
};
pub use self::swarm::{SwarmClient, SwarmStorer};
pub use self::types::{
    AccountingOf, BandwidthOf, IdentityOf, PricingOf, SpecOf, StoreOf, SwarmClientTypes,
    SwarmNetworkTypes, SwarmNodeType, SwarmPrimitives, SwarmStorerTypes, TopologyOf,
};

pub use nectar_primitives::{
    AnyChunk, Bin, Chunk, ChunkAddress, ChunkType, ChunkTypeId, ChunkTypeSet, ContentChunk,
    ProximityOrder, SingleOwnerChunk, StandardChunkSet,
};
pub use vertex_swarm_primitives::{
    BatchId, ConnectionProfile, NeighborhoodDepth, OverlayAddress, Stamp, StampedChunk,
    StorageRadius, ValidatedChunk, ValidationError, VerifiedStampedChunk,
};

// Re-export libp2p types used in config traits
pub use libp2p::Multiaddr;
