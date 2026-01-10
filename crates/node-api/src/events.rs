//! Node events and notifications

use vertex_primitives::{ChunkAddress, PeerId};
use vertex_swarm_api::{Chunk, NetworkStatus, StorageStats};

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, string::String, vec::Vec};

/// Events emitted by the Swarm node
#[derive(Debug, Clone)]
pub enum NodeEvent {
    /// Network-related event
    Network(NetworkEvent),
    /// Storage-related event
    Storage(StorageEvent),
    /// Chunk-related event
    Chunk(ChunkEvent),
    /// Node lifecycle event
    Lifecycle(LifecycleEvent),
    /// Error event
    Error(ErrorEvent),
}

/// Network-related events
#[derive(Debug, Clone)]
pub enum NetworkEvent {
    /// Node connected to the network
    Connected,
    /// Node disconnected from the network
    Disconnected,
    /// Peer connected
    PeerConnected(PeerId),
    /// Peer disconnected
    PeerDisconnected(PeerId),
    /// Network status updated
    StatusUpdated(NetworkStatus),
    /// Neighborhood depth changed
    NeighborhoodDepthChanged(u8),
}

/// Storage-related events
#[derive(Debug, Clone)]
pub enum StorageEvent {
    /// Storage statistics updated
    StatsUpdated(StorageStats),
    /// Storage capacity warning (approaching limit)
    CapacityWarning { used: u64, total: u64 },
    /// Storage pruning started
    PruningStarted,
    /// Storage pruning finished
    PruningFinished {
        chunks_removed: usize,
        bytes_freed: u64,
    },
}

/// Chunk-related events
#[derive(Debug, Clone)]
pub enum ChunkEvent {
    /// Chunk stored locally
    ChunkStored(ChunkAddress),
    /// Chunk retrieved
    ChunkRetrieved(ChunkAddress),
    /// Chunk pushed to peer
    ChunkPushed { chunk: ChunkAddress, peer: PeerId },
    /// Chunk received from peer
    ChunkReceived { chunk: ChunkAddress, peer: PeerId },
    /// Failed to store chunk
    ChunkStoreFailed { chunk: ChunkAddress, reason: String },
    /// Failed to retrieve chunk
    ChunkRetrieveFailed { chunk: ChunkAddress, reason: String },
}

/// Node lifecycle events
#[derive(Debug, Clone)]
pub enum LifecycleEvent {
    /// Node started
    Started,
    /// Node stopping
    Stopping,
    /// Node shutdown
    Shutdown,
    /// Configuration reloaded
    ConfigReloaded,
}

/// Error events
#[derive(Debug, Clone)]
pub enum ErrorEvent {
    /// Network error
    Network(String),
    /// Storage error
    Storage(String),
    /// Critical error that requires attention
    Critical(String),
}

/// A trait for event handlers
#[auto_impl::auto_impl(&, Arc)]
pub trait EventHandler: Send + Sync + 'static {
    /// Handle an event
    fn handle_event(&self, event: NodeEvent);
}

/// A simple event handler that logs events
#[derive(Debug, Clone, Default)]
pub struct LoggingEventHandler;

impl EventHandler for LoggingEventHandler {
    fn handle_event(&self, event: NodeEvent) {
        match &event {
            NodeEvent::Network(network_event) => match network_event {
                NetworkEvent::Connected => {
                    tracing::info!("Node connected to the network");
                }
                NetworkEvent::Disconnected => {
                    tracing::info!("Node disconnected from the network");
                }
                NetworkEvent::PeerConnected(peer) => {
                    tracing::debug!(?peer, "Peer connected");
                }
                NetworkEvent::PeerDisconnected(peer) => {
                    tracing::debug!(?peer, "Peer disconnected");
                }
                NetworkEvent::StatusUpdated(status) => {
                    tracing::debug!(?status, "Network status updated");
                }
                NetworkEvent::NeighborhoodDepthChanged(depth) => {
                    tracing::info!(depth, "Neighborhood depth changed");
                }
            },
            NodeEvent::Storage(storage_event) => match storage_event {
                StorageEvent::StatsUpdated(stats) => {
                    tracing::debug!(?stats, "Storage stats updated");
                }
                StorageEvent::CapacityWarning { used, total } => {
                    tracing::warn!(
                        used = %used,
                        total = %total,
                        "Storage capacity warning"
                    );
                }
                StorageEvent::PruningStarted => {
                    tracing::info!("Storage pruning started");
                }
                StorageEvent::PruningFinished {
                    chunks_removed,
                    bytes_freed,
                } => {
                    tracing::info!(chunks_removed, bytes_freed, "Storage pruning finished");
                }
            },
            NodeEvent::Chunk(chunk_event) => match chunk_event {
                ChunkEvent::ChunkStored(address) => {
                    tracing::debug!(?address, "Chunk stored locally");
                }
                ChunkEvent::ChunkRetrieved(address) => {
                    tracing::debug!(?address, "Chunk retrieved");
                }
                ChunkEvent::ChunkPushed { chunk, peer } => {
                    tracing::debug!(?chunk, ?peer, "Chunk pushed to peer");
                }
                ChunkEvent::ChunkReceived { chunk, peer } => {
                    tracing::debug!(?chunk, ?peer, "Chunk received from peer");
                }
                ChunkEvent::ChunkStoreFailed { chunk, reason } => {
                    tracing::warn!(?chunk, %reason, "Failed to store chunk");
                }
                ChunkEvent::ChunkRetrieveFailed { chunk, reason } => {
                    tracing::warn!(?chunk, %reason, "Failed to retrieve chunk");
                }
            },
            NodeEvent::Lifecycle(lifecycle_event) => match lifecycle_event {
                LifecycleEvent::Started => {
                    tracing::info!("Node started");
                }
                LifecycleEvent::Stopping => {
                    tracing::info!("Node stopping");
                }
                LifecycleEvent::Shutdown => {
                    tracing::info!("Node shutdown");
                }
                LifecycleEvent::ConfigReloaded => {
                    tracing::info!("Configuration reloaded");
                }
            },
            NodeEvent::Error(error_event) => match error_event {
                ErrorEvent::Network(error) => {
                    tracing::error!(%error, "Network error");
                }
                ErrorEvent::Storage(error) => {
                    tracing::error!(%error, "Storage error");
                }
                ErrorEvent::Critical(error) => {
                    tracing::error!(%error, "Critical error");
                }
            },
        }
    }
}

/// An event dispatcher that forwards events to registered handlers
#[derive(Debug, Clone, Default)]
pub struct EventDispatcher {
    /// Registered event handlers
    handlers: Vec<Box<dyn EventHandler>>,
}

impl EventDispatcher {
    /// Create a new event dispatcher
    pub fn new() -> Self {
        Self {
            handlers: Vec::new(),
        }
    }

    /// Register an event handler
    pub fn register_handler<H: EventHandler + 'static>(&mut self, handler: H) {
        self.handlers.push(Box::new(handler));
    }

    /// Dispatch an event to all registered handlers
    pub fn dispatch(&self, event: NodeEvent) {
        for handler in &self.handlers {
            handler.handle_event(event.clone());
        }
    }
}

impl EventHandler for EventDispatcher {
    fn handle_event(&self, event: NodeEvent) {
        self.dispatch(event);
    }
}
