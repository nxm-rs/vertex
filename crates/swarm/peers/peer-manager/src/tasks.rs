//! Background tasks for periodic persistence, cache eviction, bin replenishment, and stale peer purging.

use std::sync::Arc;
use std::time::Duration;

use tracing::debug;
use vertex_swarm_api::SwarmIdentity;
use vertex_tasks::TaskExecutor;

use crate::PeerManager;

/// Configuration for background persistence behaviour.
#[derive(Debug, Clone)]
pub struct PersistenceConfig {
    /// How often to flush the write buffer to DB.
    pub flush_interval: Duration,
    /// How often to evict non-connected peers from the hot cache.
    pub evict_interval: Duration,
    /// How often to replenish depleted proximity bins from DB.
    pub replenish_interval: Duration,
}

impl Default for PersistenceConfig {
    fn default() -> Self {
        Self {
            flush_interval: Duration::from_secs(30),
            evict_interval: Duration::from_secs(60),
            replenish_interval: Duration::from_secs(60),
        }
    }
}

/// Configuration for background stale peer purging.
#[derive(Debug, Clone)]
pub struct PurgeConfig {
    /// How often to check for stale peers.
    pub check_interval: Duration,
}

impl Default for PurgeConfig {
    fn default() -> Self {
        Self {
            check_interval: Duration::from_secs(60),
        }
    }
}

/// Spawn a background task that periodically flushes writes, evicts cold peers,
/// and replenishes depleted proximity bins.
pub fn spawn_persistence_task<I: SwarmIdentity>(
    manager: Arc<PeerManager<I>>,
    config: PersistenceConfig,
    executor: &TaskExecutor,
) {
    executor.spawn_with_graceful_shutdown_signal("peers.persistence", move |shutdown| async move {
        let mut shutdown = std::pin::pin!(shutdown);

        let mut flush_interval = tokio::time::interval(config.flush_interval);
        let mut evict_interval = tokio::time::interval(config.evict_interval);
        let mut replenish_interval = tokio::time::interval(config.replenish_interval);

        // Skip the first immediate tick
        flush_interval.tick().await;
        evict_interval.tick().await;
        replenish_interval.tick().await;

        loop {
            tokio::select! {
                guard = &mut shutdown => {
                    debug!("peer persistence shutting down, final flush");
                    manager.collect_dirty();
                    manager.flush_write_buffer();
                    drop(guard);
                    break;
                }
                _ = flush_interval.tick() => {
                    manager.collect_dirty();
                    manager.flush_write_buffer();
                }
                _ = evict_interval.tick() => {
                    manager.evict_cold();
                }
                _ = replenish_interval.tick() => {
                    manager.replenish_bins();
                }
            }
        }
    });
}

/// Spawn a background task that periodically purges stale peers.
pub fn spawn_purge_task<I: SwarmIdentity>(
    manager: Arc<PeerManager<I>>,
    config: PurgeConfig,
    executor: &TaskExecutor,
) {
    executor.spawn_periodic("peers.purge", config.check_interval, move || {
        manager.purge_stale();
    });
}
