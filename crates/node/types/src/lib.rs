//! Generic node type definitions for Vertex.
//!
//! This crate provides infrastructure-level type traits that any protocol
//! can use. It does NOT contain any protocol-specific types.
//!
//! # Type Hierarchy
//!
//! ```text
//! NodeTypes (generic infrastructure)
//!   - Database: DatabaseProvider
//!   - Rpc: RpcServer
//!   - Executor: TaskExecutor
//! ```
//!
//! Protocol-specific types (e.g., Swarm's Identity, Topology, Accounting)
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
///     type Identity: Identity;
///     type Topology: Topology;
///     type Accounting: BandwidthAccounting;
/// }
/// ```
pub trait NodeTypes: Clone + Debug + Send + Sync + 'static {
    /// Database provider for persistent state.
    type Database: DatabaseProvider;

    /// RPC server implementation.
    type Rpc: RpcServer;

    /// Task executor.
    type Executor: TaskExecutor;
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
    Db: DatabaseProvider + Debug,
    Rpc: RpcServer + Debug,
    Exec: TaskExecutor + Debug,
{
    type Database = Db;
    type Rpc = Rpc;
    type Executor = Exec;
}
