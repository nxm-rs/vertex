//! Node type definitions for Vertex Swarm
//!
//! This crate provides the foundational type system for Swarm nodes, following
//! the pattern established by reth. Types are organized in a hierarchy based
//! on node capabilities:
//!
//! # Node Type Hierarchy
//!
//! ```text
//! NodeTypes (read-only capable)
//!   - Spec, ChunkSet, Topology, Identity
//!   - DataAvailability (bandwidth/retrieval incentive)
//!          │
//!          ▼
//! PublisherNodeTypes (can write/publish)
//!   - Storage (proof for storing - stamps)
//!          │
//!          ▼
//! FullNodeTypes (stores and syncs)
//!   - Store, Sync
//! ```
//!
//! # Identity
//!
//! The [`Identity`] trait defines the interface for a node's cryptographic identity.
//! This includes the signing key, overlay address derivation, and network configuration.
//! See the [`identity`] module for details.
//!
//! # Incentive Model
//!
//! Swarm has two distinct incentive mechanisms:
//!
//! 1. **Data Availability** (retrieval incentive) - Paid to nodes that serve data.
//!    Implementations: pseudosettle (free allowance), SWAP (payment channels), both, or none.
//!
//! 2. **Storage** (storage incentive) - Proof attached to chunks when storing.
//!    Implementations: postage stamps (mainnet), or `()` for free storage (dev/private).
//!
//! # Example
//!
//! ```ignore
//! // Read-only light client
//! impl NodeTypes for ReadOnlyClient {
//!     type Spec = Hive;
//!     type ChunkSet = StandardChunkSet;
//!     type Topology = KademliaTopology;
//!     type Identity = SwarmIdentity;
//!     type DataAvailability = PseudosettleSwap;  // Uses both
//! }
//!
//! // Publisher (can also write)
//! impl PublisherNodeTypes for PublisherClient {
//!     type Storage = nectar_postage::Stamp;
//! }
//!
//! // Full node (stores and syncs)
//! impl FullNodeTypes for FullNode {
//!     type Store = RocksDbStore;
//!     type Sync = PullPushSync;
//! }
//! ```

#![cfg_attr(not(feature = "std"), no_std)]
#![warn(missing_docs)]

extern crate alloc;

mod identity;
pub use identity::Identity;

use core::fmt::Debug;
use vertex_primitives::ChunkTypeSet;
use vertex_swarm_api::{AvailabilityAccounting, ChunkSync, LocalStore, Topology};
use vertex_swarmspec::SwarmSpec;

// ============================================================================
// NodeTypes - Base for all nodes (read-only capable)
// ============================================================================

/// Base type configuration for any Swarm node.
///
/// This defines the minimum configuration needed for a node that can
/// retrieve chunks from the network. Even read-only clients need:
/// - Network specification (which swarm to connect to)
/// - Identity (cryptographic identity for the network)
/// - Chunk type support (what chunks can be handled)
/// - Topology (who to talk to)
/// - Data availability incentive (how to pay for retrieval)
#[auto_impl::auto_impl(&, Arc)]
pub trait NodeTypes: Clone + Debug + Send + Sync + Unpin + 'static {
    /// The network specification.
    ///
    /// Defines network identity (mainnet, testnet, dev), hardforks,
    /// bootnodes, and token contract address.
    type Spec: SwarmSpec + Clone;

    /// The node's cryptographic identity.
    ///
    /// Provides signing capability and overlay address derivation.
    /// The identity's `Spec` type must match `Self::Spec`.
    type Identity: Identity<Spec = Self::Spec>;

    /// The chunk types supported by this node.
    ///
    /// Determines which chunk types (content-addressed, single-owner, etc.)
    /// this node can handle.
    type ChunkSet: ChunkTypeSet;

    /// The topology implementation for peer discovery.
    ///
    /// How this node discovers and routes to peers in the network.
    type Topology: Topology + Clone;

    /// The data availability incentive mechanism.
    ///
    /// How this node pays for retrieving data (bandwidth accounting).
    /// This is a factory that creates per-peer accounting handles.
    /// Options: pseudosettle, SWAP, both, or `NoAvailabilityIncentives`.
    type DataAvailability: AvailabilityAccounting + Clone;
}

// ============================================================================
// PublisherNodeTypes - Can write/publish to the network
// ============================================================================

/// Type configuration for nodes that can publish (store) chunks.
///
/// Publishers need everything a read-only node needs, plus the ability
/// to prove payment for storage. On mainnet this means postage stamps.
pub trait PublisherNodeTypes: NodeTypes {
    /// Storage incentive proof for storing chunks.
    ///
    /// This is attached to chunks when putting them into the network.
    /// On mainnet, this is a postage stamp from a valid batch.
    /// For dev/testing, this can be `()` for free storage.
    type Storage: Send + Sync + 'static;
}

// ============================================================================
// FullNodeTypes - Stores locally and syncs
// ============================================================================

/// Type configuration for full nodes that store and sync chunks.
///
/// Full nodes participate in the network by:
/// - Storing chunks they're responsible for
/// - Syncing with neighbors to ensure data availability
///
/// They need all publisher capabilities plus storage and sync.
pub trait FullNodeTypes: PublisherNodeTypes {
    /// The local storage implementation.
    ///
    /// How this node persists chunks it's responsible for.
    type Store: LocalStore + Clone;

    /// The chunk synchronization implementation.
    ///
    /// How this node syncs chunks with its neighbors.
    type Sync: ChunkSync + Clone;
}

// ============================================================================
// Type Aliases
// ============================================================================

/// Extract the [`SwarmSpec`] type from a [`NodeTypes`] implementation.
pub type SpecOf<N> = <N as NodeTypes>::Spec;

/// Extract the [`Identity`] type from a [`NodeTypes`] implementation.
pub type IdentityOf<N> = <N as NodeTypes>::Identity;

/// Extract the [`ChunkTypeSet`] type from a [`NodeTypes`] implementation.
pub type ChunkSetOf<N> = <N as NodeTypes>::ChunkSet;

/// Extract the [`Topology`] type from a [`NodeTypes`] implementation.
pub type TopologyOf<N> = <N as NodeTypes>::Topology;

/// Extract the data availability type from a [`NodeTypes`] implementation.
pub type DataAvailabilityOf<N> = <N as NodeTypes>::DataAvailability;

/// Extract the storage incentive type from a [`PublisherNodeTypes`] implementation.
pub type StorageOf<N> = <N as PublisherNodeTypes>::Storage;

/// Extract the [`LocalStore`] type from a [`FullNodeTypes`] implementation.
pub type StoreOf<N> = <N as FullNodeTypes>::Store;

/// Extract the [`ChunkSync`] type from a [`FullNodeTypes`] implementation.
pub type SyncOf<N> = <N as FullNodeTypes>::Sync;

// ============================================================================
// Convenience Extension Traits
// ============================================================================

/// Extension trait providing convenient access to spec methods.
pub trait NodeTypesWithSpec: NodeTypes {
    /// Check if configured for mainnet.
    fn is_mainnet(spec: &Self::Spec) -> bool {
        spec.is_mainnet()
    }

    /// Check if configured for testnet.
    fn is_testnet(spec: &Self::Spec) -> bool {
        spec.is_testnet()
    }

    /// Check if configured for a development network.
    fn is_dev(spec: &Self::Spec) -> bool {
        spec.is_dev()
    }
}

// Blanket implementation
impl<N: NodeTypes> NodeTypesWithSpec for N {}

// ============================================================================
// AnyNodeTypes - Flexible Type Builder
// ============================================================================

use core::marker::PhantomData;

/// A flexible [`NodeTypes`] implementation using phantom types.
///
/// Use this when you want to specify types without creating a new struct:
///
/// ```ignore
/// type MyNode = AnyNodeTypes<Hive, SwarmIdentity, StandardChunkSet, KademliaTopology, PseudosettleSwap>;
/// ```
#[derive(Debug)]
pub struct AnyNodeTypes<Spec, Ident, ChunkSet, Topo, DA>(
    PhantomData<Spec>,
    PhantomData<Ident>,
    PhantomData<ChunkSet>,
    PhantomData<Topo>,
    PhantomData<DA>,
);

impl<Spec, Ident, ChunkSet, Topo, DA> Clone for AnyNodeTypes<Spec, Ident, ChunkSet, Topo, DA> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<Spec, Ident, ChunkSet, Topo, DA> Copy for AnyNodeTypes<Spec, Ident, ChunkSet, Topo, DA> {}

impl<Spec, Ident, ChunkSet, Topo, DA> Default for AnyNodeTypes<Spec, Ident, ChunkSet, Topo, DA> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Spec, Ident, ChunkSet, Topo, DA> AnyNodeTypes<Spec, Ident, ChunkSet, Topo, DA> {
    /// Create a new type configuration.
    pub const fn new() -> Self {
        Self(
            PhantomData,
            PhantomData,
            PhantomData,
            PhantomData,
            PhantomData,
        )
    }
}

impl<Spec, Ident, ChunkSet, Topo, DA> NodeTypes for AnyNodeTypes<Spec, Ident, ChunkSet, Topo, DA>
where
    Spec: SwarmSpec + Clone + Debug + Send + Sync + Unpin + 'static,
    Ident: Identity<Spec = Spec> + Debug + Unpin,
    ChunkSet: ChunkTypeSet + Clone + Debug + Send + Sync + Unpin + 'static,
    Topo: Topology + Clone + Debug + Send + Sync + Unpin + 'static,
    DA: AvailabilityAccounting + Clone + Debug + Send + Sync + Unpin + 'static,
{
    type Spec = Spec;
    type Identity = Ident;
    type ChunkSet = ChunkSet;
    type Topology = Topo;
    type DataAvailability = DA;
}
