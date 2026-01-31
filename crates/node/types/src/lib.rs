//! Generic node type definitions for Vertex.
//!
//! This crate provides infrastructure-level type traits that any protocol
//! can use. It does NOT contain any protocol-specific types.
//!
//! # Type Hierarchy
//!
//! ```text
//! NodeTypes (generic infrastructure)
//!   - Database: NodeDatabaseProvider
//!   - Rpc: NodeRpcServer
//!   - Executor: NodeTaskExecutor
//! ```
//!
//! Protocol-specific types (e.g., Swarm's SwarmIdentity, SwarmTopology, SwarmBandwidthAccounting)
//! should extend these in the protocol layer, not here.

#![cfg_attr(not(feature = "std"), no_std)]
#![warn(missing_docs)]

extern crate alloc;

use core::fmt::Debug;
use core::marker::PhantomData;

/// Database provider trait for node state persistence.
///
/// Implementations: redb, rocksdb, lmdb, in-memory (testing).
#[auto_impl::auto_impl(&, Arc)]
pub trait NodeDatabaseProvider: Send + Sync + Clone + 'static {}

/// RPC server trait for node API exposure.
///
/// Implementations: JSON-RPC (Bee-compatible), gRPC, none.
#[auto_impl::auto_impl(&, Arc)]
pub trait NodeRpcServer: Send + Sync + 'static {}

/// Task executor for async runtime.
pub trait NodeTaskExecutor: Send + Sync + Clone + 'static {}

/// No-op implementations for nodes without these features.
impl NodeDatabaseProvider for () {}
impl NodeRpcServer for () {}
impl NodeTaskExecutor for () {}

/// Implement NodeTaskExecutor for vertex_tasks::TaskExecutor.
impl NodeTaskExecutor for vertex_tasks::TaskExecutor {}

/// Generic node types for infrastructure components.
///
/// This trait defines the infrastructure-level types that any protocol needs.
/// Protocol-specific types (identity, topology, accounting) should be defined
/// in the protocol layer and composed with these.
///
/// # Example
///
/// ```ignore
/// // Protocol layer defines its own types:
/// pub trait SwarmNodeTypes: NodeTypes {
///     type Spec: SwarmSpec;
///     type Identity: SwarmIdentity;
///     type Topology: SwarmTopology;
///     type Accounting: SwarmBandwidthAccounting;
/// }
/// ```
pub trait NodeTypes: Clone + Debug + Send + Sync + 'static {
    /// Database provider for persistent state.
    type Database: NodeDatabaseProvider;

    /// RPC server implementation.
    type Rpc: NodeRpcServer;

    /// Task executor.
    type Executor: NodeTaskExecutor;
}

/// Type alias to extract the Database from NodeTypes.
pub type DatabaseOf<N> = <N as NodeTypes>::Database;

/// Type alias to extract the Rpc from NodeTypes.
pub type RpcOf<N> = <N as NodeTypes>::Rpc;

/// Type alias to extract the Executor from NodeTypes.
pub type ExecutorOf<N> = <N as NodeTypes>::Executor;

/// Flexible NodeTypes using phantom types.
///
/// Use when you need NodeTypes without creating a new struct.
///
/// # Example
///
/// ```ignore
/// type MyNodeTypes = AnyNodeTypes<MyDb, MyRpc, MyExecutor>;
/// ```
#[derive(Debug)]
pub struct AnyNodeTypes<Db = (), Rpc = (), Exec = ()>(PhantomData<(Db, Rpc, Exec)>);

impl<Db, Rpc, Exec> Clone for AnyNodeTypes<Db, Rpc, Exec> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<Db, Rpc, Exec> Copy for AnyNodeTypes<Db, Rpc, Exec> {}

impl<Db, Rpc, Exec> Default for AnyNodeTypes<Db, Rpc, Exec> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<Db, Rpc, Exec> NodeTypes for AnyNodeTypes<Db, Rpc, Exec>
where
    Db: NodeDatabaseProvider + Debug,
    Rpc: NodeRpcServer + Debug,
    Exec: NodeTaskExecutor + Debug,
{
    type Database = Db;
    type Rpc = Rpc;
    type Executor = Exec;
}
