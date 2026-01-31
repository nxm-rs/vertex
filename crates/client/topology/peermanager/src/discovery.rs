//! Peer discovery channel for decoupled peer storage.
//!
//! This module provides a channel-based approach for handling discovered peers:
//! - Hive protocol sends [`SwarmPeer`] events to a broadcast channel
//! - A background task consumes events and persists to the PeerStore
//! - Kademlia receives overlays separately (synchronously in the event loop)
//!
//! This decouples the hive protocol from persistence concerns.

use std::sync::Arc;

use tokio::sync::broadcast;
use tracing::{debug, trace, warn};
use vertex_swarm_peer::SwarmPeer;

use crate::PeerManager;

/// Channel capacity for peer discovery events.
///
/// Should be large enough to handle bursts from hive broadcasts.
pub const DISCOVERY_CHANNEL_CAPACITY: usize = 1024;

/// Sender half of the peer discovery channel.
pub type DiscoverySender = broadcast::Sender<SwarmPeer>;

/// Receiver half of the peer discovery channel.
pub type DiscoveryReceiver = broadcast::Receiver<SwarmPeer>;

/// Create a new peer discovery channel.
///
/// Returns the sender (for the event loop) and a receiver (for the consumer task).
pub fn discovery_channel() -> (DiscoverySender, DiscoveryReceiver) {
    let (tx, rx) = broadcast::channel(DISCOVERY_CHANNEL_CAPACITY);
    (tx, rx)
}

/// Run the peer store consumer task.
///
/// This task receives discovered peers from the channel and persists them
/// to the peer store. It runs until the channel is closed (sender dropped).
///
/// # Arguments
///
/// * `peer_manager` - The peer manager with an attached store
/// * `rx` - Receiver for discovered peer events
pub async fn run_peer_store_consumer(peer_manager: Arc<PeerManager>, mut rx: DiscoveryReceiver) {
    debug!("peer store consumer task started");

    let mut batch: Vec<SwarmPeer> = Vec::with_capacity(64);
    let mut persist_count = 0u64;

    loop {
        // Try to receive, batching multiple events if available
        match rx.recv().await {
            Ok(peer) => {
                batch.push(peer);

                // Drain any additional pending events (non-blocking)
                while let Ok(peer) = rx.try_recv() {
                    batch.push(peer);
                    if batch.len() >= 64 {
                        break;
                    }
                }

                // Persist the batch
                if !batch.is_empty() {
                    trace!(count = batch.len(), "persisting discovered peers batch");
                    peer_manager.store_hive_peers_batch(batch.drain(..));
                    persist_count += batch.len() as u64;
                }
            }
            Err(broadcast::error::RecvError::Closed) => {
                debug!(
                    total_persisted = persist_count,
                    "peer discovery channel closed, consumer task exiting"
                );
                break;
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                warn!(
                    skipped,
                    "peer store consumer lagged, some peers may not be persisted"
                );
                // Continue processing - we'll catch up
            }
        }
    }

    // Final flush
    if let Err(e) = peer_manager.flush() {
        warn!(error = %e, "failed to flush peer store on consumer exit");
    } else {
        debug!("peer store flushed on consumer exit");
    }
}
