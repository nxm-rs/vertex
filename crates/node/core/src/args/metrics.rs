//! Metrics CLI arguments.

use std::net::{IpAddr, SocketAddr};

use clap::Args;
use serde::{Deserialize, Serialize};
use vertex_observability::MetricsServerConfig;

use crate::constants::{DEFAULT_LOCALHOST_ADDR, DEFAULT_METRICS_PORT};

/// Default prefix for all metrics.
const DEFAULT_METRICS_PREFIX: &str = "vertex";

/// Default upkeep interval in seconds.
const DEFAULT_UPKEEP_INTERVAL_SECS: u64 = 5;

/// Prometheus metrics configuration.
#[derive(Debug, Args, Clone, Serialize, Deserialize)]
#[command(next_help_heading = "Metrics")]
#[serde(default)]
pub struct MetricsArgs {
    /// Enable the prometheus metrics HTTP endpoint.
    #[arg(long = "metrics", id = "metrics.enabled")]
    pub enabled: bool,

    /// Metrics listen address.
    #[arg(long = "metrics.addr", id = "metrics.addr", default_value = DEFAULT_LOCALHOST_ADDR)]
    pub addr: String,

    /// Metrics listen port.
    #[arg(long = "metrics.port", id = "metrics.port", default_value_t = DEFAULT_METRICS_PORT)]
    pub port: u16,

    /// Prefix for all metrics.
    #[arg(long = "metrics.prefix", id = "metrics.prefix", default_value = DEFAULT_METRICS_PREFIX)]
    pub prefix: String,

    /// How often to run recorder upkeep in seconds.
    #[arg(long = "metrics.upkeep-interval", id = "metrics.upkeep-interval", default_value_t = DEFAULT_UPKEEP_INTERVAL_SECS)]
    pub upkeep_interval_secs: u64,
}

impl Default for MetricsArgs {
    fn default() -> Self {
        Self {
            enabled: false,
            addr: DEFAULT_LOCALHOST_ADDR.to_string(),
            port: DEFAULT_METRICS_PORT,
            prefix: DEFAULT_METRICS_PREFIX.to_string(),
            upkeep_interval_secs: DEFAULT_UPKEEP_INTERVAL_SECS,
        }
    }
}

impl MetricsArgs {
    /// Build metrics server config.
    ///
    /// Returns None if metrics are disabled.
    pub fn metrics_config(&self) -> Option<MetricsServerConfig> {
        if !self.enabled {
            return None;
        }

        let ip: IpAddr = self.addr.parse().unwrap_or_else(|_| {
            tracing::warn!(
                addr = %self.addr,
                "Invalid metrics address, falling back to localhost"
            );
            IpAddr::from([127, 0, 0, 1])
        });
        let addr = SocketAddr::new(ip, self.port);

        Some(MetricsServerConfig::new(
            addr,
            self.prefix.clone(),
            self.upkeep_interval_secs,
        ))
    }
}
