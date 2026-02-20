//! Configuration for the identify behaviour.

use libp2p::identity::PublicKey;
use std::time::Duration;

/// Configuration for the identify behaviour.
#[derive(Debug, Clone)]
pub struct Config {
    pub(crate) local_public_key: PublicKey,
    pub(crate) protocol_version: String,
    pub(crate) agent_version: String,
    pub(crate) interval: Duration,
    pub(crate) push_listen_addr_updates: bool,
    pub(crate) hide_listen_addrs: bool,
    pub(crate) cache_size: usize,
}

impl Config {
    /// Create a new config with the given public key.
    pub fn new(local_public_key: PublicKey) -> Self {
        Self {
            local_public_key,
            protocol_version: crate::PROTOCOL_VERSION.to_string(),
            agent_version: crate::AGENT_VERSION.to_string(),
            interval: Duration::from_secs(5 * 60),
            push_listen_addr_updates: false,
            hide_listen_addrs: false,
            cache_size: 100,
        }
    }

    /// Set the protocol version string.
    pub fn with_protocol_version(mut self, version: impl Into<String>) -> Self {
        self.protocol_version = version.into();
        self
    }

    /// Set the agent version string.
    pub fn with_agent_version(mut self, version: impl Into<String>) -> Self {
        self.agent_version = version.into();
        self
    }

    /// Set the interval between periodic identify exchanges.
    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }

    /// Auto-push when listen addresses change.
    pub fn with_push_listen_addr_updates(mut self, enabled: bool) -> Self {
        self.push_listen_addr_updates = enabled;
        self
    }

    /// Hide listen addresses (only send external addresses).
    pub fn with_hide_listen_addrs(mut self, hide: bool) -> Self {
        self.hide_listen_addrs = hide;
        self
    }

    /// Set the peer address cache size.
    pub fn with_cache_size(mut self, size: usize) -> Self {
        self.cache_size = size;
        self
    }
}
