//! Access control traits (authentication, authorization, accounting)
//!
//! This module defines the traits for controlling access to chunks in the Swarm network.

use alloc::boxed::Box;
use async_trait::async_trait;
use core::fmt::Debug;
use vertex_primitives::{ChunkAddress, Result};

use crate::chunk::Chunk;

/// Trait for access control credentials
pub trait Credential: Clone + Debug + Send + Sync + 'static {}

/// Authentication trait for verifying chunk credentials
#[auto_impl::auto_impl(&, Arc)]
pub trait Authenticator: Send + Sync + 'static {
    /// The credential type used by this authenticator
    type Credential: Credential;

    /// Authenticate a chunk with the given credential
    fn authenticate(&self, chunk: &dyn Chunk, credential: &Self::Credential) -> Result<()>;

    /// Authenticate a retrieval request with optional credential
    fn authenticate_retrieval(
        &self,
        address: &ChunkAddress,
        credential: Option<&Self::Credential>,
    ) -> Result<()>;
}

/// Authorization trait for determining if chunks can be stored or retrieved
#[auto_impl::auto_impl(&, Arc)]
pub trait Authorizer: Send + Sync + 'static {
    /// Authorize a chunk to be stored
    fn authorize_storage(&self, chunk: &dyn Chunk) -> Result<()>;

    /// Authorize a chunk to be retrieved
    fn authorize_retrieval(&self, address: &ChunkAddress) -> Result<()>;
}

/// Accounting trait for tracking resource usage
#[auto_impl::auto_impl(&, Arc)]
pub trait Accountant: Send + Sync + 'static {
    /// Record that a chunk is being stored
    fn record_storage(&self, chunk: &dyn Chunk) -> Result<()>;

    /// Record that a chunk is being retrieved
    fn record_retrieval(&self, address: &ChunkAddress) -> Result<()>;

    /// Get available storage capacity
    fn available_capacity(&self) -> usize;

    /// Check if storage capacity is available for a chunk of the given size
    fn has_capacity_for(&self, size: usize) -> bool;
}

/// Combined access control for chunks
#[auto_impl::auto_impl(&, Arc)]
pub trait AccessController: Send + Sync + 'static {
    /// The credential type used by this controller
    type Credential: Credential;

    /// Check if a chunk can be stored with the given credential
    fn check_storage_permission(
        &self,
        chunk: &dyn Chunk,
        credential: &Self::Credential,
    ) -> Result<()>;

    /// Check if a chunk can be retrieved
    fn check_retrieval_permission(
        &self,
        address: &ChunkAddress,
        credential: Option<&Self::Credential>,
    ) -> Result<()>;

    /// Record that a chunk has been stored
    fn record_storage(&self, chunk: &dyn Chunk) -> Result<()>;

    /// Record that a chunk has been retrieved
    fn record_retrieval(&self, address: &ChunkAddress) -> Result<()>;
}

/// Factory for creating access controllers
#[auto_impl::auto_impl(&, Arc)]
pub trait AccessControllerFactory: Send + Sync + 'static {
    /// The type of access controller this factory creates
    type Controller: AccessController;

    /// Create a new access controller
    fn create_controller(&self, config: &AccessConfig) -> Result<Self::Controller>;
}

/// Configuration for access control
#[derive(Debug, Clone)]
pub struct AccessConfig {
    /// Maximum chunk size allowed
    pub max_chunk_size: usize,
    /// Maximum storage capacity
    pub max_capacity: u64,
    /// Whether to enforce strict authentication
    pub strict_authentication: bool,
    /// Authentication parameters
    pub auth_params: Vec<(String, String)>,
}
