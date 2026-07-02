//! Peer management CLI arguments and validated configuration.

#[cfg(feature = "cli")]
use clap::Args;
#[cfg(feature = "cli")]
use serde::{Deserialize, Serialize};
use vertex_swarm_api::{
    DEFAULT_PEER_BAN_THRESHOLD, DEFAULT_PEER_MAX_PER_BIN, DEFAULT_PEER_WARN_THRESHOLD,
    PeerConfigValues,
};

/// Validated peer management configuration.
#[derive(Debug, Clone)]
pub struct PeerConfig {
    ban_threshold: f64,
    warn_threshold: f64,
    max_per_bin: usize,
}

impl Default for PeerConfig {
    fn default() -> Self {
        Self {
            ban_threshold: DEFAULT_PEER_BAN_THRESHOLD,
            warn_threshold: DEFAULT_PEER_WARN_THRESHOLD,
            max_per_bin: DEFAULT_PEER_MAX_PER_BIN,
        }
    }
}

#[cfg(feature = "cli")]
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
        }
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
#[cfg(feature = "cli")]
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
}

#[cfg(feature = "cli")]
impl Default for PeerArgs {
    fn default() -> Self {
        Self {
            ban_threshold: DEFAULT_PEER_BAN_THRESHOLD,
            warn_threshold: DEFAULT_PEER_WARN_THRESHOLD,
            max_per_bin: DEFAULT_PEER_MAX_PER_BIN,
        }
    }
}

#[cfg(feature = "cli")]
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
