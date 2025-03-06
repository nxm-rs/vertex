//! Protocol-related traits

use crate::{Chunk, Result};
use vertex_primitives::{ChunkAddress, PeerId, ProtocolId};

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, string::String, vec::Vec};

/// Core trait for protocol handlers
#[auto_impl::auto_impl(&, Arc)]
pub trait ProtocolHandler: Send + Sync + 'static {
    /// The protocol ID
    fn protocol_id(&self) -> ProtocolId;

    /// Get the protocol name
    fn protocol_name(&self) -> &str;

    /// Get the protocol version
    fn protocol_version(&self) -> &str;
}

/// Handler for retrieval protocol
#[async_trait]
#[auto_impl::auto_impl(&, Arc)]
pub trait RetrievalProtocol: ProtocolHandler {
    /// Retrieve a chunk from a peer
    async fn retrieve_from(
        &self,
        peer: &PeerId,
        address: &ChunkAddress,
    ) -> Result<Box<dyn Chunk>>;

    /// Handle a retrieval request from a peer
    async fn handle_retrieval_request(
        &self,
        peer: &PeerId,
        address: &ChunkAddress,
    ) -> Result<Box<dyn Chunk>>;
}

/// Handler for push sync protocol
#[async_trait]
#[auto_impl::auto_impl(&, Arc)]
pub trait PushSyncProtocol: ProtocolHandler {
    /// Push a chunk to a peer
    async fn push_to(
        &self,
        peer: &PeerId,
        chunk: Box<dyn Chunk>,
    ) -> Result<()>;

    /// Handle a push request from a peer
    async fn handle_push_request(
        &self,
        peer: &PeerId,
        chunk: Box<dyn Chunk>,
    ) -> Result<()>;
}

/// Handler for pull sync protocol
#[async_trait]
#[auto_impl::auto_impl(&, Arc)]
pub trait PullSyncProtocol: ProtocolHandler {
    /// Sync a batch of chunks from a peer
    async fn pull_from(
        &self,
        peer: &PeerId,
        start: &ChunkAddress,
        limit: usize,
    ) -> Result<Vec<Box<dyn Chunk>>>;

    /// Handle a pull request from a peer
    async fn handle_pull_request(
        &self,
        peer: &PeerId,
        start: &ChunkAddress,
        limit: usize,
    ) -> Result<Vec<Box<dyn Chunk>>>;
}

/// Factory for creating protocol handlers
#[auto_impl::auto_impl(&, Arc)]
pub trait ProtocolFactory: Send + Sync + 'static {
    /// Create a retrieval protocol handler
    fn create_retrieval_protocol(&self) -> Box<dyn RetrievalProtocol>;

    /// Create a push sync protocol handler
    fn create_push_protocol(&self) -> Box<dyn PushSyncProtocol>;

    /// Create a pull sync protocol handler
    fn create_pull_protocol(&self) -> Box<dyn PullSyncProtocol>;
}
