//! Thin periodic driver for [`PeerManager::tick`].
//!
//! The peer manager owns no timers; this driver runs the tick on the task
//! executor's clock. Spawned from the node launch path. The final snapshot
//! on graceful shutdown is written by topology, which sees the shutdown
//! signal first.

use std::sync::Arc;
use std::time::Duration;

use tracing::info;
use vertex_swarm_api::SwarmIdentity;
use vertex_tasks::TaskExecutor;

use crate::PeerManager;
use crate::entry::unix_timestamp_secs;

/// Default interval between maintenance ticks.
///
/// Each tick purges stale peers; snapshots are written only when
/// [`crate::PeerManagerConfig::snapshot_interval`] has elapsed.
pub const DEFAULT_TICK_INTERVAL: Duration = Duration::from_secs(60);

/// Spawn the periodic maintenance task driving [`PeerManager::tick`].
pub fn spawn_peer_manager_task<I: SwarmIdentity>(
    manager: Arc<PeerManager<I>>,
    tick_interval: Duration,
    executor: &TaskExecutor,
) {
    info!(
        interval_secs = tick_interval.as_secs(),
        "peer manager maintenance task started"
    );
    executor.spawn_periodic("peers.tick", tick_interval, move || {
        manager.tick(unix_timestamp_secs());
    });
}
