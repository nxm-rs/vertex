//! Common types used throughout the Vertex API
//!
//! This module defines additional types that are used by multiple components.

use alloc::{string::String, vec::Vec};
use core::fmt::Debug;

/// Neighborhood parameters
#[derive(Debug, Clone)]
pub struct NeighborhoodParams {
    /// Current neighborhood depth/radius
    pub depth: u8,
    /// Minimum chunk count threshold
    pub min_chunks: usize,
    /// Target chunk count
    pub target_chunks: usize,
    /// Maximum chunk count threshold
    pub max_chunks: usize,
    /// Saturation threshold (percentage)
    pub saturation_threshold: f32,
}

/// Events that can be emitted by the node
#[derive(Debug, Clone)]
pub enum SwarmEvent {
    /// Network topology changed
    NetworkTopologyChanged {
        /// New connected peer count
        peer_count: usize,
        /// New neighborhood depth
        depth: u8,
    },

    /// Chunk stored locally
    ChunkStored {
        /// Chunk address
        address: String,
        /// Chunk size
        size: usize,
    },

    /// Chunk retrieved
    ChunkRetrieved {
        /// Chunk address
        address: String,
        /// Retrieve latency in ms
        latency_ms: u64,
    },

    /// Full sync progress
    SyncProgress {
        /// Chunks synced so far
        synced_chunks: usize,
        /// Total chunks to sync
        total_chunks: usize,
        /// Percentage complete
        percent_complete: f32,
    },

    /// Bandwidth accounting event
    BandwidthEvent {
        /// Peer ID
        peer: String,
        /// Amount in bytes
        bytes: u64,
        /// Direction (incoming/outgoing)
        direction: String,
    },

    /// Payment event
    PaymentEvent {
        /// Peer ID
        peer: String,
        /// Amount
        amount: u64,
        /// Event type
        event_type: String,
    },

    /// Error event
    ErrorEvent {
        /// Error message
        message: String,
        /// Component that generated the error
        component: String,
        /// Severity level
        severity: String,
    },
}

/// Subscribe handler for events
#[auto_impl::auto_impl(&, Arc)]
pub trait EventSubscriber: Send + Sync + 'static {
    /// Subscribe to events
    fn subscribe(&self) -> Box<dyn Iterator<Item = SwarmEvent> + Send + '_>;

    /// Publish an event
    fn publish(&self, event: SwarmEvent);
}

/// API request/response types
#[derive(Debug, Clone)]
pub enum ApiRequestType {
    /// Store a chunk
    StoreChunk,
    /// Retrieve a chunk
    RetrieveChunk,
    /// Get node status
    Status,
    /// Get network topology
    Topology,
    /// Get peers
    Peers,
    /// Get bandwidth status
    Bandwidth,
    /// Get storage stats
    Storage,
    /// Node administration
    Admin,
    /// Debug/metrics
    Debug,
}

/// API authentication methods
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiAuthMethod {
    /// No authentication
    None,
    /// API key
    ApiKey(String),
    /// JWT token
    Jwt(String),
    /// Basic authentication
    Basic {
        /// Username
        username: String,
        /// Password
        password: String,
    },
}

/// Service metadata
#[derive(Debug, Clone)]
pub struct ServiceInfo {
    /// Service name
    pub name: String,
    /// Service version
    pub version: String,
    /// Git commit hash
    pub git_hash: String,
    /// Build timestamp
    pub build_timestamp: String,
    /// Build features
    pub features: Vec<String>,
}

/// Chain connection information
#[derive(Debug, Clone)]
pub struct ChainConnection {
    /// Chain ID
    pub chain_id: u64,
    /// RPC endpoint
    pub endpoint: String,
    /// Block number
    pub block_number: u64,
    /// Connection status
    pub connected: bool,
}
