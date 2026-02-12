//! Background pruning task for PeerManager.

use std::sync::Arc;
use std::time::Duration;

use vertex_tasks::TaskSpawnerExt;

use crate::PeerManager;

/// Configuration for background peer pruning.
#[derive(Debug, Clone)]
pub struct PruneConfig {
    /// How often to check if pruning is needed.
    pub check_interval: Duration,
    /// Trigger pruning when utilization exceeds this ratio (0.0-1.0).
    pub capacity_threshold: f64,
    /// Target utilization after pruning (0.0-1.0).
    pub target_utilization: f64,
    /// Number of peers to remove per batch (yields between batches).
    pub batch_size: usize,
}

impl Default for PruneConfig {
    fn default() -> Self {
        Self {
            check_interval: Duration::from_secs(60),
            capacity_threshold: 0.9,
            target_utilization: 0.5,
            batch_size: 100,
        }
    }
}

/// Spawn a background task that periodically prunes excess peers.
pub fn spawn_prune_task(
    manager: Arc<PeerManager>,
    config: PruneConfig,
    executor: &impl TaskSpawnerExt,
) {
    executor.spawn_with_graceful_shutdown_signal("peer_pruner", move |shutdown| async move {
        let mut shutdown = std::pin::pin!(shutdown);

        loop {
            tokio::select! {
                guard = &mut shutdown => {
                    tracing::debug!("peer pruner shutting down");
                    drop(guard);
                    break;
                }
                () = tokio::time::sleep(config.check_interval) => {
                    if manager.should_prune(&config) {
                        manager.prune_async(&config).await;
                    }
                }
            }
        }
    });
}
