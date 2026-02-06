//! Peer management CLI arguments and validated configuration.

use std::path::PathBuf;

use clap::Args;
use serde::{Deserialize, Serialize};
use vertex_swarm_api::{PeerConfigValues, DEFAULT_PEER_BAN_THRESHOLD, DEFAULT_PEER_STORE_LIMIT};

/// Validated peer management configuration.
#[derive(Debug, Clone)]
pub struct PeerConfig {
    ban_threshold: f64,
    store_limit: Option<usize>,
    store_path: Option<PathBuf>,
}

impl Default for PeerConfig {
    fn default() -> Self {
        Self {
            ban_threshold: DEFAULT_PEER_BAN_THRESHOLD,
            store_limit: Some(DEFAULT_PEER_STORE_LIMIT),
            store_path: None,
        }
    }
}

impl From<&PeerArgs> for PeerConfig {
    fn from(args: &PeerArgs) -> Self {
        Self {
            ban_threshold: args.ban_threshold,
            store_limit: if args.store_limit == 0 {
                None
            } else {
                Some(args.store_limit)
            },
            store_path: args.store_path.clone(),
        }
    }
}

impl PeerConfigValues for PeerConfig {
    fn ban_threshold(&self) -> f64 {
        self.ban_threshold
    }

    fn store_limit(&self) -> Option<usize> {
        self.store_limit
    }

    fn store_path(&self) -> Option<PathBuf> {
        self.store_path.clone()
    }
}

/// Peer management configuration.
#[derive(Debug, Clone, Args, Serialize, Deserialize)]
#[serde(default)]
pub struct PeerArgs {
    /// Score threshold below which peers are banned.
    #[arg(long = "network.peer.ban-threshold", default_value_t = DEFAULT_PEER_BAN_THRESHOLD)]
    pub ban_threshold: f64,

    /// Maximum peers to track (0 = unlimited).
    #[arg(long = "network.peer.store-limit", default_value_t = DEFAULT_PEER_STORE_LIMIT)]
    pub store_limit: usize,

    /// Path for peer store persistence.
    #[arg(long = "network.peer.store-path", value_name = "PATH")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_path: Option<PathBuf>,
}

impl Default for PeerArgs {
    fn default() -> Self {
        Self {
            ban_threshold: DEFAULT_PEER_BAN_THRESHOLD,
            store_limit: DEFAULT_PEER_STORE_LIMIT,
            store_path: None,
        }
    }
}

impl PeerConfigValues for PeerArgs {
    fn ban_threshold(&self) -> f64 {
        self.ban_threshold
    }

    fn store_limit(&self) -> Option<usize> {
        if self.store_limit == 0 {
            None
        } else {
            Some(self.store_limit)
        }
    }

    fn store_path(&self) -> Option<PathBuf> {
        self.store_path.clone()
    }
}
