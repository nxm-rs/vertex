//! Peer management CLI arguments and validated configuration.

use clap::Args;
use serde::{Deserialize, Serialize};
use vertex_swarm_api::{
    DEFAULT_PEER_BAN_THRESHOLD, DEFAULT_PEER_MAX_PER_BIN, DEFAULT_PEER_WARN_THRESHOLD,
    PeerConfigValues,
};
use vertex_swarm_peer_manager::IpTrackerConfig;

/// Default live per-IP concurrent-connection cap on the CLI; `0` is unlimited,
/// the off-by-default state that mirrors `IpTrackerConfig::DEFAULT_MAX_CONNECTIONS_PER_IP`.
const DEFAULT_MAX_PER_IP: usize = 0;

/// Default distinct-overlay cap per IP for the identity-cycling detector.
const DEFAULT_MAX_OVERLAYS_PER_IP: usize = IpTrackerConfig::DEFAULT_MAX_OVERLAYS_PER_IP;

/// Validated peer management configuration.
#[derive(Debug, Clone)]
pub struct PeerConfig {
    ban_threshold: f64,
    warn_threshold: f64,
    max_per_bin: usize,
    /// Live per-IP concurrent-connection cap; `None` is unlimited.
    max_connections_per_ip: Option<usize>,
    /// Distinct-overlay cap per IP for the identity-cycling detector.
    max_overlays_per_ip: usize,
}

impl Default for PeerConfig {
    fn default() -> Self {
        Self {
            ban_threshold: DEFAULT_PEER_BAN_THRESHOLD,
            warn_threshold: DEFAULT_PEER_WARN_THRESHOLD,
            max_per_bin: DEFAULT_PEER_MAX_PER_BIN,
            // Off by default: `0` means unlimited, matching the library default.
            max_connections_per_ip: (DEFAULT_MAX_PER_IP != 0).then_some(DEFAULT_MAX_PER_IP),
            max_overlays_per_ip: DEFAULT_MAX_OVERLAYS_PER_IP,
        }
    }
}

impl From<&PeerArgs> for PeerConfig {
    fn from(args: &PeerArgs) -> Self {
        Self {
            ban_threshold: args.ban_threshold,
            warn_threshold: args.warn_threshold,
            max_per_bin: if args.max_per_bin == 0 {
                DEFAULT_PEER_MAX_PER_BIN
            } else {
                args.max_per_bin
            },
            // `0` on the connection cap means unlimited.
            max_connections_per_ip: (args.max_per_ip != 0).then_some(args.max_per_ip),
            max_overlays_per_ip: if args.max_overlays_per_ip == 0 {
                DEFAULT_MAX_OVERLAYS_PER_IP
            } else {
                args.max_overlays_per_ip
            },
        }
    }
}

impl PeerConfig {
    /// Live per-IP concurrent-connection cap; `None` is unlimited.
    #[must_use]
    pub fn max_connections_per_ip(&self) -> Option<usize> {
        self.max_connections_per_ip
    }

    /// Distinct-overlay cap per IP for the identity-cycling detector.
    #[must_use]
    pub fn max_overlays_per_ip(&self) -> usize {
        self.max_overlays_per_ip
    }
}

impl PeerConfigValues for PeerConfig {
    fn ban_threshold(&self) -> f64 {
        self.ban_threshold
    }

    fn warn_threshold(&self) -> f64 {
        self.warn_threshold
    }

    fn max_per_bin(&self) -> usize {
        self.max_per_bin
    }
}

/// Peer management configuration.
#[derive(Debug, Clone, Args, Serialize, Deserialize)]
#[serde(default)]
pub struct PeerArgs {
    /// Score threshold below which peers are banned.
    #[arg(long = "network.peer.ban-threshold", default_value_t = DEFAULT_PEER_BAN_THRESHOLD)]
    pub ban_threshold: f64,

    /// Score threshold below which a warning is emitted.
    #[arg(long = "network.peer.warn-threshold", default_value_t = DEFAULT_PEER_WARN_THRESHOLD)]
    pub warn_threshold: f64,

    /// Maximum peers per proximity bin (0 = default 128).
    #[arg(long = "network.peer.max-per-bin", default_value_t = DEFAULT_PEER_MAX_PER_BIN)]
    pub max_per_bin: usize,

    /// Maximum live concurrent connections admitted from one IP (0 =
    /// unlimited). Sized for legitimate high-density IPs (servers running
    /// many nodes, NAT/CGNAT farms). Local-subnet and trusted peers are
    /// always exempt.
    #[arg(long = "network.peer.max-per-ip", default_value_t = DEFAULT_MAX_PER_IP)]
    pub max_per_ip: usize,

    /// Distinct overlays tolerated per IP before the identity-cycling
    /// detector scores newcomers down (0 = default 128).
    #[arg(long = "network.peer.max-overlays-per-ip", default_value_t = DEFAULT_MAX_OVERLAYS_PER_IP)]
    pub max_overlays_per_ip: usize,
}

impl Default for PeerArgs {
    fn default() -> Self {
        Self {
            ban_threshold: DEFAULT_PEER_BAN_THRESHOLD,
            warn_threshold: DEFAULT_PEER_WARN_THRESHOLD,
            max_per_bin: DEFAULT_PEER_MAX_PER_BIN,
            max_per_ip: DEFAULT_MAX_PER_IP,
            max_overlays_per_ip: DEFAULT_MAX_OVERLAYS_PER_IP,
        }
    }
}

impl PeerArgs {
    /// Live per-IP concurrent-connection cap; `None` (from `0`) is unlimited.
    #[must_use]
    pub fn max_connections_per_ip(&self) -> Option<usize> {
        (self.max_per_ip != 0).then_some(self.max_per_ip)
    }

    /// Distinct-overlay cap per IP for the identity-cycling detector.
    #[must_use]
    pub fn max_overlays_per_ip(&self) -> usize {
        if self.max_overlays_per_ip == 0 {
            DEFAULT_MAX_OVERLAYS_PER_IP
        } else {
            self.max_overlays_per_ip
        }
    }
}

impl PeerConfigValues for PeerArgs {
    fn ban_threshold(&self) -> f64 {
        self.ban_threshold
    }

    fn warn_threshold(&self) -> f64 {
        self.warn_threshold
    }

    fn max_per_bin(&self) -> usize {
        if self.max_per_bin == 0 {
            DEFAULT_PEER_MAX_PER_BIN
        } else {
            self.max_per_bin
        }
    }
}
