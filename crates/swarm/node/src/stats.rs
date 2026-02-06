//! Node statistics reporting task.
//!
//! Provides periodic logging of node health and topology statistics.

use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;
use tracing::info;
use vertex_swarm_api::SwarmTopology;
use vertex_tasks::TaskExecutor;

const DEFAULT_STATS_INTERVAL: Duration = Duration::from_secs(20);

/// Stats reporter configuration.
#[derive(Debug, Clone)]
pub struct StatsConfig {
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
    pub fn with_interval(interval: Duration) -> Self {
        Self { interval }
    }
}

/// Spawns a background task that periodically reports node statistics.
pub fn spawn_stats_task<T: SwarmTopology + 'static>(
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

fn log_stats<T: SwarmTopology>(topology: &T) {
    let connected = topology.connected_peers_count();
    let known = topology.known_peers_count();
    let depth = topology.depth();
    let pending = topology.pending_connections_count();

    let bin_sizes = topology.bin_sizes();
    let mut bin_summary = String::new();
    for (po, (conn, known_in_bin)) in bin_sizes.iter().enumerate() {
        if *conn > 0 || *known_in_bin > 0 {
            if !bin_summary.is_empty() {
                bin_summary.push(' ');
            }
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
    use std::sync::Arc;
    use nectar_primitives::ChunkAddress;
    use vertex_swarm_identity::Identity;
    use vertex_swarm_primitives::{OverlayAddress, SwarmNodeType};

    struct MockTopology {
        identity: Arc<Identity>,
        connected: usize,
        known: usize,
        depth: u8,
    }

    impl MockTopology {
        fn new(connected: usize, known: usize, depth: u8) -> Self {
            Self {
                identity: Arc::new(Identity::random(
                    vertex_swarm_spec::init_testnet(),
                    SwarmNodeType::Client,
                )),
                connected,
                known,
                depth,
            }
        }
    }

    impl SwarmTopology for MockTopology {
        type Identity = Arc<Identity>;

        fn identity(&self) -> &Self::Identity {
            &self.identity
        }

        fn depth(&self) -> u8 {
            self.depth
        }

        fn neighbors(&self, _depth: u8) -> Vec<OverlayAddress> {
            vec![]
        }

        fn closest_to(&self, _address: &ChunkAddress, _count: usize) -> Vec<OverlayAddress> {
            vec![]
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
        let topology = MockTopology::new(0, 0, 0);
        log_stats(&topology);
    }
}
