//! Node statistics reporting task.
//!
//! Provides periodic logging of node health and topology statistics.

use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;
use tracing::info;
use vertex_swarm_api::TopologyProvider;
use vertex_tasks::TaskExecutor;

/// Default interval for stats reporting.
const DEFAULT_STATS_INTERVAL: Duration = Duration::from_secs(20);

/// Stats reporter configuration.
#[derive(Debug, Clone)]
pub struct StatsConfig {
    /// Interval between stats reports.
    pub interval: Duration,
}

impl Default for StatsConfig {
    fn default() -> Self {
        Self {
            interval: DEFAULT_STATS_INTERVAL,
        }
    }
}

impl StatsConfig {
    /// Create config with custom interval.
    pub fn with_interval(interval: Duration) -> Self {
        Self { interval }
    }
}

/// Spawns a background task that periodically reports node statistics.
///
/// The task logs at `info` level, providing operator-friendly stats including:
/// - Connected peer count
/// - Known peer count
/// - Kademlia depth
/// - Pending connections
///
/// # Arguments
///
/// * `topology` - The topology provider to query for stats
/// * `config` - Stats reporting configuration
/// * `executor` - Task executor for spawning
pub fn spawn_stats_task<T: TopologyProvider>(
    topology: Arc<T>,
    config: StatsConfig,
    executor: &TaskExecutor,
) -> JoinHandle<()> {
    executor.spawn_with_graceful_shutdown_signal("node_stats", |shutdown| async move {
        let mut shutdown = std::pin::pin!(shutdown);

        loop {
            tokio::select! {
                guard = &mut shutdown => {
                    tracing::debug!("stats task shutting down");
                    drop(guard);
                    break;
                }
                _ = tokio::time::sleep(config.interval) => {
                    log_stats(&*topology);
                }
            }
        }
    })
}

/// Log current node statistics at info level.
fn log_stats<T: TopologyProvider>(topology: &T) {
    let connected = topology.connected_peers_count();
    let known = topology.known_peers_count();
    let depth = topology.depth();
    let pending = topology.pending_connections_count();

    // Build compact bin summary showing non-empty bins
    let bin_sizes = topology.bin_sizes();
    let mut bin_summary = String::new();
    for (po, (conn, known_in_bin)) in bin_sizes.iter().enumerate() {
        if *conn > 0 || *known_in_bin > 0 {
            if !bin_summary.is_empty() {
                bin_summary.push(' ');
            }
            // Mark depth boundary with brackets
            if po as u8 == depth {
                bin_summary.push_str(&format!("[{po}:{conn}/{known_in_bin}]"));
            } else {
                bin_summary.push_str(&format!("{po}:{conn}/{known_in_bin}"));
            }
        }
    }

    if bin_summary.is_empty() {
        bin_summary = "(empty)".to_string();
    }

    info!(
        connected,
        known,
        depth,
        pending,
        bins = %bin_summary,
        "swarm status"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockTopology {
        connected: usize,
        known: usize,
        depth: u8,
    }

    impl TopologyProvider for MockTopology {
        fn overlay_address(&self) -> String {
            "00".repeat(32)
        }

        fn depth(&self) -> u8 {
            self.depth
        }

        fn connected_peers_count(&self) -> usize {
            self.connected
        }

        fn known_peers_count(&self) -> usize {
            self.known
        }

        fn pending_connections_count(&self) -> usize {
            0
        }

        fn bin_sizes(&self) -> Vec<(usize, usize)> {
            vec![(0, 0); 32]
        }

        fn connected_peers_in_bin(&self, _po: u8) -> Vec<String> {
            vec![]
        }
    }

    #[test]
    fn test_config_default() {
        let config = StatsConfig::default();
        assert_eq!(config.interval, Duration::from_secs(20));
    }

    #[test]
    fn test_log_stats_empty() {
        let topology = MockTopology {
            connected: 0,
            known: 0,
            depth: 0,
        };
        // Just verify it doesn't panic
        log_stats(&topology);
    }
}
