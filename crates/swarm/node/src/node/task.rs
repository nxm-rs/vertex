//! Background tasks for node operations.

use std::sync::Arc;

use vertex_swarm_api::{SwarmTopologyState, SwarmTopologyStats};
use vertex_swarm_peer_manager::ScoreDistribution;
use vertex_tasks::TaskExecutor;

use super::stats::{StatsConfig, log_stats};

/// Spawns a background task that periodically reports node statistics.
pub fn spawn_stats_task<T: SwarmTopologyState + SwarmTopologyStats + 'static>(
    topology: Arc<T>,
    score_distribution: Arc<ScoreDistribution>,
    config: StatsConfig,
    executor: &TaskExecutor,
) {
    executor.spawn_periodic("node.stats", config.interval, move || {
        log_stats(&*topology);
        score_distribution.push_gauges();
    });
}
