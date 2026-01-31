//! Swarm type hierarchy for different node capabilities.
//!
//! Capability levels: SwarmBootnodeTypes → SwarmClientTypes → SwarmStorerTypes.

use crate::{
    SwarmBandwidthAccounting, SwarmClientAccounting, SwarmIdentity, SwarmLocalStore,
    SwarmTopology,
};
use vertex_tasks::SpawnableTask;
use vertex_node_types::NodeTypes;
use vertex_swarmspec::SwarmSpec;

pub use vertex_swarm_primitives::SwarmNodeType;

/// Base types for network participation (bootnodes).
///
/// Bootnodes help peers discover each other but don't retrieve or store data.
/// This is the root of the capability hierarchy.
pub trait SwarmBootnodeTypes: Clone + Send + Sync + Unpin + 'static {
    /// Network specification (network ID, hardforks, chunk types).
    type Spec: SwarmSpec + Clone;

    /// Cryptographic identity for handshake and routing.
    type Identity: SwarmIdentity<Spec = Self::Spec>;

    /// Peer discovery and routing.
    type Topology: SwarmTopology<Identity = Self::Identity>;

    /// The Swarm node event loop type.
    ///
    /// This is the main P2P networking task that handles peer connections,
    /// protocol messages, and network events.
    type Node: SpawnableTask;

    /// The client service type for handling chunk requests.
    type ClientService: SpawnableTask;

    /// Handle type for interacting with the client service.
    type ClientHandle: Clone + Send + Sync + 'static;
}

/// Types for client nodes that can retrieve and upload chunks.
///
/// Extends bootnodes with client accounting (pricing + bandwidth).
pub trait SwarmClientTypes: SwarmBootnodeTypes {
    /// Combined pricing and bandwidth accounting for client operations.
    type Accounting: SwarmClientAccounting<
        Bandwidth: SwarmBandwidthAccounting<Identity = <Self as SwarmBootnodeTypes>::Identity>,
    >;
}

/// Types for storer nodes that store chunks locally.
///
/// Extends client nodes with local storage.
pub trait SwarmStorerTypes: SwarmClientTypes {
    /// Local chunk storage.
    type Store: SwarmLocalStore + Clone;
}

/// Swarm node types combining bootnode capability with node infrastructure.
///
/// This trait is automatically implemented for any type that implements
/// both `SwarmBootnodeTypes` and `NodeTypes`.
pub trait SwarmNodeTypes: SwarmBootnodeTypes + NodeTypes {}
impl<T: SwarmBootnodeTypes + NodeTypes> SwarmNodeTypes for T {}

/// Swarm node types for client capability with infrastructure.
///
/// Automatically implemented for types implementing `SwarmClientTypes + NodeTypes`.
pub trait SwarmClientNodeTypes: SwarmClientTypes + NodeTypes {}
impl<T: SwarmClientTypes + NodeTypes> SwarmClientNodeTypes for T {}

/// Swarm node types for storer capability with infrastructure.
///
/// Automatically implemented for types implementing `SwarmStorerTypes + NodeTypes`.
pub trait SwarmStorerNodeTypes: SwarmStorerTypes + NodeTypes {}
impl<T: SwarmStorerTypes + NodeTypes> SwarmStorerNodeTypes for T {}

/// Extract the Spec type from SwarmBootnodeTypes.
pub type SpecOf<T> = <T as SwarmBootnodeTypes>::Spec;

/// Extract the Identity type from SwarmBootnodeTypes.
pub type IdentityOf<T> = <T as SwarmBootnodeTypes>::Identity;

/// Extract the Topology type from SwarmBootnodeTypes.
pub type TopologyOf<T> = <T as SwarmBootnodeTypes>::Topology;

/// Extract the Node type from SwarmBootnodeTypes.
pub type NodeOf<T> = <T as SwarmBootnodeTypes>::Node;

/// Extract the ClientService type from SwarmBootnodeTypes.
pub type ClientServiceOf<T> = <T as SwarmBootnodeTypes>::ClientService;

/// Extract the ClientHandle type from SwarmBootnodeTypes.
pub type ClientHandleOf<T> = <T as SwarmBootnodeTypes>::ClientHandle;

/// Extract the Accounting type from SwarmClientTypes.
pub type AccountingOf<T> = <T as SwarmClientTypes>::Accounting;

/// Extract the Bandwidth type from SwarmClientTypes.
pub type BandwidthOf<T> =
    <<T as SwarmClientTypes>::Accounting as SwarmClientAccounting>::Bandwidth;

/// Extract the Pricing type from SwarmClientTypes.
pub type PricingOf<T> = <<T as SwarmClientTypes>::Accounting as SwarmClientAccounting>::Pricing;

/// Extract the Store type from SwarmStorerTypes.
pub type StoreOf<T> = <T as SwarmStorerTypes>::Store;
