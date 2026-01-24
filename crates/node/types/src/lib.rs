//! Node type definitions for Vertex Swarm.
//!
//! This crate wraps SwarmTypes from swarm-api and adds node infrastructure:
//!
//! ```text
//! SwarmTypes (swarm-api)          NodeTypes (node-types)
//! ──────────────────────          ──────────────────────
//! BootnodeTypes                   NodeTypes
//! LightTypes          ────►         + Swarm (any SwarmTypes)
//! PublisherTypes                    + Database
//! FullTypes                         + Rpc
//!                                   + Executor
//! ```
//!
//! This separation allows:
//! - Same Swarm logic with different databases (redb, rocksdb, in-memory)
//! - Same Swarm logic with different RPC (JSON-RPC, gRPC, none)
//! - Testing with mock infrastructure

#![cfg_attr(not(feature = "std"), no_std)]
#![warn(missing_docs)]

extern crate alloc;

use core::fmt::Debug;

// Re-export SwarmTypes hierarchy from swarm-api
pub use vertex_swarm_api::{
    BootnodeTypes, FullTypes, Identity, LightTypes, PublisherTypes,
    AccountingOf, IdentityOf, SpecOf, StorageOf, StoreOf, SyncOf, TopologyOf,
};

/// Database provider trait for node state persistence.
///
/// Implementations: redb, rocksdb, lmdb, in-memory (testing).
#[auto_impl::auto_impl(&, Arc)]
pub trait DatabaseProvider: Send + Sync + Clone + 'static {}

/// RPC server trait for node API exposure.
///
/// Implementations: JSON-RPC (Bee-compatible), gRPC, none.
#[auto_impl::auto_impl(&, Arc)]
pub trait RpcServer: Send + Sync + 'static {}

/// Task executor for async runtime.
pub trait TaskExecutor: Send + Sync + Clone + 'static {}

/// No-op implementations for nodes without these features.
impl DatabaseProvider for () {}
impl RpcServer for () {}
impl TaskExecutor for () {}

/// Implement TaskExecutor for vertex_tasks::TaskExecutor.
impl TaskExecutor for vertex_tasks::TaskExecutor {}

/// Node types combining Swarm layer with infrastructure.
pub trait NodeTypes: Clone + Debug + Send + Sync + 'static {
    /// Network specification.
    type Spec: vertex_swarmspec::SwarmSpec + Clone;

    /// Cryptographic identity.
    type Identity: Identity<Spec = Self::Spec>;

    /// Peer topology.
    type Topology: vertex_swarm_api::Topology + Clone;

    /// Availability accounting.
    type Accounting: vertex_swarm_api::AvailabilityAccounting;

    /// Database provider for persistent state.
    type Database: DatabaseProvider;

    /// RPC server implementation.
    type Rpc: RpcServer;

    /// Task executor.
    type Executor: TaskExecutor;
}

/// Node types for publisher capability.
pub trait PublisherNodeTypes: NodeTypes {
    /// Storage proof type (postage stamps on mainnet, `()` for dev).
    type Storage: Send + Sync + 'static;
}

/// Node types for full node capability.
pub trait FullNodeTypes: PublisherNodeTypes {
    /// Local chunk storage.
    type Store: vertex_swarm_api::LocalStore + Clone;

    /// Chunk synchronization.
    type Sync: vertex_swarm_api::ChunkSync + Clone;
}

/// Type alias to extract Spec from NodeTypes.
pub type NodeSpecOf<N> = <N as NodeTypes>::Spec;

/// Type alias to extract Identity from NodeTypes.
pub type NodeIdentityOf<N> = <N as NodeTypes>::Identity;

/// Type alias to extract Topology from NodeTypes.
pub type NodeTopologyOf<N> = <N as NodeTypes>::Topology;

/// Type alias to extract Accounting from NodeTypes.
pub type NodeAccountingOf<N> = <N as NodeTypes>::Accounting;

/// Type alias to extract the Database from NodeTypes.
pub type DatabaseOf<N> = <N as NodeTypes>::Database;

/// Type alias to extract the Rpc from NodeTypes.
pub type RpcOf<N> = <N as NodeTypes>::Rpc;

/// Type alias to extract the Executor from NodeTypes.
pub type ExecutorOf<N> = <N as NodeTypes>::Executor;

/// Type alias to extract Storage from PublisherNodeTypes.
pub type NodeStorageOf<N> = <N as PublisherNodeTypes>::Storage;

/// Type alias to extract Store from FullNodeTypes.
pub type NodeStoreOf<N> = <N as FullNodeTypes>::Store;

/// Type alias to extract Sync from FullNodeTypes.
pub type NodeSyncOf<N> = <N as FullNodeTypes>::Sync;

// ============================================================================
// AnyNodeTypes - Flexible Type Builder
// ============================================================================

use core::marker::PhantomData;

/// Flexible NodeTypes using phantom types.
///
/// Use when you need NodeTypes without creating a new struct.
#[derive(Debug)]
pub struct AnyNodeTypes<Spec, Ident, Topo, Acct, Db = (), Rpc = (), Exec = ()>(
    PhantomData<(Spec, Ident, Topo, Acct, Db, Rpc, Exec)>,
);

impl<Spec, Ident, Topo, Acct, Db, Rpc, Exec> Clone
    for AnyNodeTypes<Spec, Ident, Topo, Acct, Db, Rpc, Exec>
{
    fn clone(&self) -> Self {
        *self
    }
}

impl<Spec, Ident, Topo, Acct, Db, Rpc, Exec> Copy
    for AnyNodeTypes<Spec, Ident, Topo, Acct, Db, Rpc, Exec>
{
}

impl<Spec, Ident, Topo, Acct, Db, Rpc, Exec> Default
    for AnyNodeTypes<Spec, Ident, Topo, Acct, Db, Rpc, Exec>
{
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<Spec, Ident, Topo, Acct, Db, Rpc, Exec> NodeTypes
    for AnyNodeTypes<Spec, Ident, Topo, Acct, Db, Rpc, Exec>
where
    Spec: vertex_swarmspec::SwarmSpec + Clone + Debug + Send + Sync + Unpin + 'static,
    Ident: Identity<Spec = Spec> + Debug + Unpin,
    Topo: vertex_swarm_api::Topology + Clone + Debug + Send + Sync + Unpin + 'static,
    Acct: vertex_swarm_api::AvailabilityAccounting + Debug + Send + Sync + Unpin + 'static,
    Db: DatabaseProvider + Debug,
    Rpc: RpcServer + Debug,
    Exec: TaskExecutor + Debug,
{
    type Spec = Spec;
    type Identity = Ident;
    type Topology = Topo;
    type Accounting = Acct;
    type Database = Db;
    type Rpc = Rpc;
    type Executor = Exec;
}
