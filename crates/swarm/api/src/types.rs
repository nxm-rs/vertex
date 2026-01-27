//! Swarm type hierarchy for different node capabilities.
//!
//! Types are organized by capability level, with `BootnodeTypes` as the root:
//!
//! ```text
//! BootnodeTypes (network participation only)
//!   - Spec, Identity, Topology, Node, ClientService, ClientHandle
//!        │
//!        ▼
//! LightTypes (can retrieve)
//!   - + Accounting (availability incentives)
//!        │
//!        ▼
//! PublisherTypes (can upload)
//!   - + Storage (postage stamps)
//!        │
//!        ▼
//! FullTypes (stores and syncs)
//!   - + Store, Sync
//! ```
//!
//! For node infrastructure integration, combined traits add `NodeTypes`:
//!
//! ```text
//! SwarmNodeTypes = BootnodeTypes + NodeTypes
//! SwarmLightNodeTypes = LightTypes + NodeTypes
//! SwarmPublisherNodeTypes = PublisherTypes + NodeTypes
//! SwarmFullNodeTypes = FullTypes + NodeTypes
//! ```

use crate::{
    BandwidthAccounting, ChunkSync, LocalStore, RunnableClientService, RunnableNode, Topology,
};
use alloy_primitives::{Address, B256};
use alloy_signer::{Signer, SignerSync};
use core::fmt::Debug;
use nectar_primitives::SwarmAddress;
use std::sync::Arc;
use vertex_net_primitives_traits::calculate_overlay_address;
use vertex_node_types::NodeTypes;
use vertex_swarmspec::SwarmSpec;

/// Identity for Swarm network participation.
///
/// Provides the cryptographic identity needed for handshake authentication
/// and overlay address derivation.
#[auto_impl::auto_impl(&, Arc, Box)]
pub trait Identity: Clone + Debug + Send + Sync + 'static {
    /// The network specification type.
    type Spec: SwarmSpec + Clone;

    /// The signer type for signing handshake messages.
    type Signer: Signer + SignerSync + Clone + Send + Sync + 'static;

    /// Get the network specification.
    fn spec(&self) -> &Self::Spec;

    /// Get the nonce for overlay address derivation.
    fn nonce(&self) -> B256;

    /// Get the signer for handshake authentication.
    fn signer(&self) -> Arc<Self::Signer>;

    /// Whether this node operates as a full node.
    fn is_full_node(&self) -> bool;

    /// Optional welcome message for peers (max 140 chars).
    fn welcome_message(&self) -> Option<&str> {
        Some("Buzzing in from the Rustacean hive")
    }

    /// Ethereum address derived from the signing key.
    fn ethereum_address(&self) -> Address {
        self.signer().address()
    }

    /// Overlay address for Kademlia routing.
    ///
    /// Computed as: `keccak256(ethereum_address || network_id || nonce)`
    fn overlay_address(&self) -> SwarmAddress {
        calculate_overlay_address(
            &self.ethereum_address(),
            self.spec().network_id(),
            &self.nonce(),
        )
    }
}

/// Base types for network participation (bootnodes).
///
/// Bootnodes help peers discover each other but don't retrieve or store data.
/// This is the root of the capability hierarchy.
pub trait BootnodeTypes: Clone + Debug + Send + Sync + Unpin + 'static {
    /// Network specification (network ID, hardforks, chunk types).
    type Spec: SwarmSpec + Clone;

    /// Cryptographic identity for handshake and routing.
    type Identity: Identity<Spec = Self::Spec>;

    /// Peer discovery and routing.
    type Topology: Topology + Clone + Debug;

    /// The Swarm node event loop type.
    ///
    /// This is the main P2P networking task that handles peer connections,
    /// protocol messages, and network events.
    type Node: RunnableNode;

    /// The client service type for handling chunk requests.
    type ClientService: RunnableClientService;

    /// Handle type for interacting with the client service.
    type ClientHandle: Clone + Send + Sync + 'static;
}

/// Types for light nodes that can retrieve chunks.
///
/// Extends bootnodes with bandwidth accounting for retrieval incentives.
pub trait LightTypes: BootnodeTypes {
    /// Bandwidth accounting for retrieval incentives (pseudosettle/SWAP).
    type Accounting: BandwidthAccounting + Debug;
}

/// Types for publisher nodes that can upload chunks.
///
/// Extends light nodes with storage proof capability (postage stamps).
pub trait PublisherTypes: LightTypes {
    /// Storage proof type (postage stamps on mainnet, `()` for dev).
    type Storage: Send + Sync + Debug + 'static;
}

/// Types for full nodes that store and sync chunks.
///
/// Extends publishers with local storage and synchronization.
pub trait FullTypes: PublisherTypes {
    /// Local chunk storage.
    type Store: LocalStore + Clone + Debug;

    /// Chunk synchronization with neighbors.
    type Sync: ChunkSync + Clone + Debug;
}

/// Swarm node types combining bootnode capability with node infrastructure.
///
/// This trait is automatically implemented for any type that implements
/// both `BootnodeTypes` and `NodeTypes`.
pub trait SwarmNodeTypes: BootnodeTypes + NodeTypes {}
impl<T: BootnodeTypes + NodeTypes> SwarmNodeTypes for T {}

/// Swarm node types for light node capability with infrastructure.
///
/// Automatically implemented for types implementing `LightTypes + NodeTypes`.
pub trait SwarmLightNodeTypes: LightTypes + NodeTypes {}
impl<T: LightTypes + NodeTypes> SwarmLightNodeTypes for T {}

/// Swarm node types for publisher capability with infrastructure.
///
/// Automatically implemented for types implementing `PublisherTypes + NodeTypes`.
pub trait SwarmPublisherNodeTypes: PublisherTypes + NodeTypes {}
impl<T: PublisherTypes + NodeTypes> SwarmPublisherNodeTypes for T {}

/// Swarm node types for full node capability with infrastructure.
///
/// Automatically implemented for types implementing `FullTypes + NodeTypes`.
pub trait SwarmFullNodeTypes: FullTypes + NodeTypes {}
impl<T: FullTypes + NodeTypes> SwarmFullNodeTypes for T {}

/// Extract the Spec type from BootnodeTypes.
pub type SpecOf<T> = <T as BootnodeTypes>::Spec;

/// Extract the Identity type from BootnodeTypes.
pub type IdentityOf<T> = <T as BootnodeTypes>::Identity;

/// Extract the Topology type from BootnodeTypes.
pub type TopologyOf<T> = <T as BootnodeTypes>::Topology;

/// Extract the Node type from BootnodeTypes.
pub type NodeOf<T> = <T as BootnodeTypes>::Node;

/// Extract the ClientService type from BootnodeTypes.
pub type ClientServiceOf<T> = <T as BootnodeTypes>::ClientService;

/// Extract the ClientHandle type from BootnodeTypes.
pub type ClientHandleOf<T> = <T as BootnodeTypes>::ClientHandle;

/// Extract the Accounting type from LightTypes.
pub type AccountingOf<T> = <T as LightTypes>::Accounting;

/// Extract the Storage type from PublisherTypes.
pub type StorageOf<T> = <T as PublisherTypes>::Storage;

/// Extract the Store type from FullTypes.
pub type StoreOf<T> = <T as FullTypes>::Store;

/// Extract the Sync type from FullTypes.
pub type SyncOf<T> = <T as FullTypes>::Sync;
