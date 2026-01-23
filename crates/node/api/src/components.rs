//! Runtime component instances for nodes.
//!
//! These structs hold the actual instances of the types defined in the node type traits.
//! They bridge the type-level configuration from [`vertex_node_types`] with
//! actual runtime instances.
//!
//! # Hierarchy
//!
//! ```text
//! NodeComponents (read-only)
//!   - swarm (SwarmReader), topology
//!          │
//!          ▼
//! PublisherComponents (can write)
//!   - swarm (SwarmWriter), topology
//!          │
//!          ▼
//! FullNodeComponents (stores and syncs)
//!   - swarm, topology, store, sync
//! ```
//!
//! # Type Enforcement
//!
//! Components enforce that the Swarm implementation matches the NodeTypes:
//! - `S::Accounting = N::DataAvailability` - bandwidth accounting must match
//! - `S::Storage = N::Storage` - storage incentive proof must match

use vertex_node_types::{FullNodeTypes, NodeTypes, PublisherNodeTypes};
use vertex_swarm_api::{ChunkSync, LocalStore, SwarmReader, SwarmWriter, Topology};

// ============================================================================
// NodeComponents - Read-only capable nodes
// ============================================================================

/// Components for a read-only node.
///
/// Uses [`SwarmReader`] - can only retrieve chunks, not store them.
/// Enforces that the swarm's accounting type matches the node's data availability type.
#[derive(Debug, Clone)]
pub struct NodeComponents<N, S>
where
    N: NodeTypes,
    S: SwarmReader<Accounting = N::DataAvailability>,
{
    /// The swarm implementation for get operations.
    pub swarm: S,
    /// The topology implementation for peer discovery.
    pub topology: N::Topology,
}

impl<N, S> NodeComponents<N, S>
where
    N: NodeTypes,
    S: SwarmReader<Accounting = N::DataAvailability>,
{
    /// Create new read-only node components.
    pub fn new(swarm: S, topology: N::Topology) -> Self {
        Self { swarm, topology }
    }

    /// Get the swarm client.
    pub fn swarm(&self) -> &S {
        &self.swarm
    }

    /// Get the topology.
    pub fn topology(&self) -> &N::Topology {
        &self.topology
    }

    /// Get the data availability (bandwidth accounting) handler.
    ///
    /// This is accessed via the swarm's accounting factory.
    pub fn accounting(&self) -> &S::Accounting {
        self.swarm.accounting()
    }
}

// ============================================================================
// PublisherComponents - Can publish chunks
// ============================================================================

/// Components for a publisher node.
///
/// Uses [`SwarmWriter`] - can both retrieve and store chunks.
/// Enforces that:
/// - `S::Accounting = N::DataAvailability` - bandwidth accounting matches
/// - `S::Storage = N::Storage` - storage incentive proof matches
#[derive(Debug, Clone)]
pub struct PublisherComponents<N, S>
where
    N: PublisherNodeTypes,
    S: SwarmWriter<Accounting = N::DataAvailability, Storage = N::Storage>,
{
    /// The swarm implementation for put/get operations.
    pub swarm: S,
    /// The topology implementation for peer discovery.
    pub topology: N::Topology,
}

impl<N, S> PublisherComponents<N, S>
where
    N: PublisherNodeTypes,
    S: SwarmWriter<Accounting = N::DataAvailability, Storage = N::Storage>,
{
    /// Create new publisher node components.
    pub fn new(swarm: S, topology: N::Topology) -> Self {
        Self { swarm, topology }
    }

    /// Get the swarm client.
    pub fn swarm(&self) -> &S {
        &self.swarm
    }

    /// Get the topology.
    pub fn topology(&self) -> &N::Topology {
        &self.topology
    }

    /// Get the data availability (bandwidth accounting) handler.
    pub fn accounting(&self) -> &S::Accounting {
        self.swarm.accounting()
    }
}

// ============================================================================
// FullNodeComponents - Stores locally and syncs
// ============================================================================

/// Components for a full node.
///
/// Full nodes store chunks locally and sync with neighbors.
/// They include all publisher capabilities plus storage and sync.
#[derive(Debug, Clone)]
pub struct FullNodeComponents<N, S>
where
    N: FullNodeTypes,
    S: SwarmWriter<Accounting = N::DataAvailability, Storage = N::Storage>,
{
    /// The swarm implementation for put/get operations.
    pub swarm: S,
    /// The topology implementation for peer discovery.
    pub topology: N::Topology,
    /// The local storage implementation.
    pub store: N::Store,
    /// The chunk sync implementation.
    pub sync: N::Sync,
}

impl<N, S> FullNodeComponents<N, S>
where
    N: FullNodeTypes,
    S: SwarmWriter<Accounting = N::DataAvailability, Storage = N::Storage>,
{
    /// Create new full node components.
    pub fn new(swarm: S, topology: N::Topology, store: N::Store, sync: N::Sync) -> Self {
        Self {
            swarm,
            topology,
            store,
            sync,
        }
    }

    /// Get the swarm client.
    pub fn swarm(&self) -> &S {
        &self.swarm
    }

    /// Get the topology.
    pub fn topology(&self) -> &N::Topology {
        &self.topology
    }

    /// Get the data availability (bandwidth accounting) handler.
    pub fn accounting(&self) -> &S::Accounting {
        self.swarm.accounting()
    }

    /// Get the local store.
    pub fn store(&self) -> &N::Store {
        &self.store
    }

    /// Get the sync handler.
    pub fn sync(&self) -> &N::Sync {
        &self.sync
    }
}

// ============================================================================
// Component Access Traits
// ============================================================================

/// Trait for types that provide read-only swarm access.
pub trait HasSwarmReader {
    /// The Swarm implementation type.
    type Swarm: SwarmReader;

    /// Get the swarm client.
    fn swarm(&self) -> &Self::Swarm;

    /// Get the bandwidth accounting factory.
    fn accounting(&self) -> &<Self::Swarm as SwarmReader>::Accounting {
        self.swarm().accounting()
    }
}

/// Trait for types that provide read-write swarm access.
pub trait HasSwarmWriter: HasSwarmReader
where
    Self::Swarm: SwarmWriter,
{
    // Inherits swarm() and accounting() from HasSwarmReader
}

impl<N, S> HasSwarmReader for NodeComponents<N, S>
where
    N: NodeTypes,
    S: SwarmReader<Accounting = N::DataAvailability>,
{
    type Swarm = S;

    fn swarm(&self) -> &S {
        &self.swarm
    }
}

impl<N, S> HasSwarmReader for PublisherComponents<N, S>
where
    N: PublisherNodeTypes,
    S: SwarmWriter<Accounting = N::DataAvailability, Storage = N::Storage>,
{
    type Swarm = S;

    fn swarm(&self) -> &S {
        &self.swarm
    }
}

impl<N, S> HasSwarmWriter for PublisherComponents<N, S>
where
    N: PublisherNodeTypes,
    S: SwarmWriter<Accounting = N::DataAvailability, Storage = N::Storage>,
{
}

impl<N, S> HasSwarmReader for FullNodeComponents<N, S>
where
    N: FullNodeTypes,
    S: SwarmWriter<Accounting = N::DataAvailability, Storage = N::Storage>,
{
    type Swarm = S;

    fn swarm(&self) -> &S {
        &self.swarm
    }
}

impl<N, S> HasSwarmWriter for FullNodeComponents<N, S>
where
    N: FullNodeTypes,
    S: SwarmWriter<Accounting = N::DataAvailability, Storage = N::Storage>,
{
}

/// Trait for types that provide access to topology.
pub trait HasTopology {
    /// The Topology implementation type.
    type Topology: Topology;

    /// Get the topology.
    fn topology(&self) -> &Self::Topology;
}

impl<N, S> HasTopology for NodeComponents<N, S>
where
    N: NodeTypes,
    S: SwarmReader<Accounting = N::DataAvailability>,
{
    type Topology = N::Topology;

    fn topology(&self) -> &N::Topology {
        &self.topology
    }
}

impl<N, S> HasTopology for PublisherComponents<N, S>
where
    N: PublisherNodeTypes,
    S: SwarmWriter<Accounting = N::DataAvailability, Storage = N::Storage>,
{
    type Topology = N::Topology;

    fn topology(&self) -> &N::Topology {
        &self.topology
    }
}

impl<N, S> HasTopology for FullNodeComponents<N, S>
where
    N: FullNodeTypes,
    S: SwarmWriter<Accounting = N::DataAvailability, Storage = N::Storage>,
{
    type Topology = N::Topology;

    fn topology(&self) -> &N::Topology {
        &self.topology
    }
}

/// Trait for types that provide access to local storage.
pub trait HasStore {
    /// The LocalStore implementation type.
    type Store: LocalStore;

    /// Get the local store.
    fn store(&self) -> &Self::Store;
}

impl<N, S> HasStore for FullNodeComponents<N, S>
where
    N: FullNodeTypes,
    S: SwarmWriter<Accounting = N::DataAvailability, Storage = N::Storage>,
{
    type Store = N::Store;

    fn store(&self) -> &N::Store {
        &self.store
    }
}

/// Trait for types that provide access to chunk sync.
pub trait HasSync {
    /// The ChunkSync implementation type.
    type Sync: ChunkSync;

    /// Get the sync handler.
    fn sync(&self) -> &Self::Sync;
}

impl<N, S> HasSync for FullNodeComponents<N, S>
where
    N: FullNodeTypes,
    S: SwarmWriter<Accounting = N::DataAvailability, Storage = N::Storage>,
{
    type Sync = N::Sync;

    fn sync(&self) -> &N::Sync {
        &self.sync
    }
}
