//! Swarm type hierarchy for different node capabilities.
//!
//! Capability levels: SwarmPrimitives → SwarmNetworkTypes → SwarmClientTypes → SwarmStorerTypes.

use crate::{
    SwarmIdentity, SwarmLocalStore, SwarmSpec, SwarmTopologyPeers, SwarmTopologyRouting,
    SwarmTopologyState, SwarmTopologyStats,
};

pub use vertex_swarm_primitives::SwarmNodeType;

/// Pure data types for Swarm network participation.
///
/// Contains only configuration and identity data, no services.
/// Use this when you need spec/identity without a running topology.
pub trait SwarmPrimitives: Send + Sync + 'static {
    /// Network specification (network ID, hardforks, chunk types).
    type Spec: SwarmSpec;

    /// Cryptographic identity for handshake and routing.
    type Identity: SwarmIdentity<Spec = Self::Spec>;
}

/// Types for nodes participating in the network overlay.
///
/// Extends primitives with topology (peer discovery service).
pub trait SwarmNetworkTypes: SwarmPrimitives {
    /// Peer discovery and routing.
    type Topology: SwarmTopologyState<Identity = <Self as SwarmPrimitives>::Identity>
        + SwarmTopologyRouting
        + SwarmTopologyPeers
        + SwarmTopologyStats;
}

/// Types for client nodes that can retrieve and upload chunks.
///
/// Extends network types with accounting (pricing + bandwidth).
pub trait SwarmClientTypes: SwarmNetworkTypes {
    /// Combined pricing and bandwidth accounting for client operations.
    type Accounting: Send + Sync;
}

/// Types for storer nodes that store chunks locally.
///
/// Extends client nodes with local storage.
pub trait SwarmStorerTypes: SwarmClientTypes {
    /// Local chunk storage.
    type Store: SwarmLocalStore;
}

/// Extract the Spec type from SwarmPrimitives.
pub type SpecOf<T> = <T as SwarmPrimitives>::Spec;

/// Extract the Identity type from SwarmPrimitives.
pub type IdentityOf<T> = <T as SwarmPrimitives>::Identity;

/// Extract the Topology type from SwarmNetworkTypes.
pub type TopologyOf<T> = <T as SwarmNetworkTypes>::Topology;

/// Extract the Accounting type from SwarmClientTypes.
pub type AccountingOf<T> = <T as SwarmClientTypes>::Accounting;

/// Extract the Store type from SwarmStorerTypes.
pub type StoreOf<T> = <T as SwarmStorerTypes>::Store;
