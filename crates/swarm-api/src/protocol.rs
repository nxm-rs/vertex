//! Protocol implementation traits
//!
//! This module defines the traits for protocol handling in the Swarm network.

use alloc::{boxed::Box, string::String, vec::Vec};
use async_trait::async_trait;
use core::fmt::Debug;
use vertex_primitives::{ChunkAddress, PeerId, Result};

use crate::chunk::Chunk;

/// Core trait for protocol handlers
#[auto_impl::auto_impl(&, Arc)]
pub trait ProtocolHandler: Send + Sync + 'static {
    /// The protocol ID
    fn protocol_id(&self) -> String;

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

/// Handler for peer exchange protocol
#[async_trait]
#[auto_impl::auto_impl(&, Arc)]
pub trait PeerExchangeProtocol: ProtocolHandler {
    /// Request peers from a node
    async fn request_peers(
        &self,
        peer: &PeerId,
        count: usize,
    ) -> Result<Vec<(PeerId, Vec<String>)>>;

    /// Handle a peer exchange request
    async fn handle_peer_request(
        &self,
        peer: &PeerId,
        count: usize,
    ) -> Result<Vec<(PeerId, Vec<String>)>>;
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

    /// Create a peer exchange protocol handler
    fn create_peer_exchange_protocol(&self) -> Box<dyn PeerExchangeProtocol>;
}

/// Protocol message types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolMessageType {
    /// Retrieval request
    RetrievalRequest,
    /// Retrieval response
    RetrievalResponse,
    /// Push request
    PushRequest,
    /// Push response
    PushResponse,
    /// Pull request
    PullRequest,
    /// Pull response
    PullResponse,
    /// Peer exchange request
    PeerExchangeRequest,
    /// Peer exchange response
    PeerExchangeResponse,
    /// Ping request
    Ping,
    /// Pong response
    Pong,
    /// Handshake
    Handshake,
    /// Custom message type
    Custom(u8),
}

/// Protocol version
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolVersion {
    /// Major version
    pub major: u8,
    /// Minor version
    pub minor: u8,
    /// Patch version
    pub patch: u8,
}

impl ProtocolVersion {
    /// Create a new protocol version
    pub const fn new(major: u8, minor: u8, patch: u8) -> Self {
        Self { major, minor, patch }
    }

    /// Format version as string
    pub fn to_string(&self) -> String {
        format!("{}.{}.{}", self.major, self.minor, self.patch)
    }
}
