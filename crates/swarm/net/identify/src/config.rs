//! Configuration for the identify behaviour.

use libp2p::identity::PublicKey;
use std::time::Duration;

/// Configuration for the identify behaviour.
#[derive(Debug, Clone)]
pub struct Config {
    pub(crate) local_public_key: PublicKey,
    pub(crate) protocol_version: String,
    pub(crate) agent_version: String,
    /// Interval between periodic identify exchanges. `None` disables periodic
    /// re-identification (initial exchange on connection still happens).
    pub(crate) interval: Option<Duration>,
    pub(crate) push_listen_addr_updates: bool,
    pub(crate) hide_listen_addrs: bool,
    pub(crate) cache_size: usize,
    /// Metrics label identifying which swarm this behaviour belongs to.
    pub(crate) purpose: &'static str,
}

impl Config {
    /// Create a new config with the given public key.
    ///
    /// Periodic re-identification is disabled by default. The initial identify
    /// exchange still happens on every new connection. Reactive pushes (protocol
    /// changes, targeted address push) are unaffected.
    pub fn new(local_public_key: PublicKey) -> Self {
        Self {
            local_public_key,
            protocol_version: crate::PROTOCOL_VERSION.to_string(),
            agent_version: crate::AGENT_VERSION.to_string(),
            interval: None,
            push_listen_addr_updates: false,
            hide_listen_addrs: false,
            cache_size: 100,
            purpose: "topology",
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
    ///
    /// `None` disables periodic re-identification (default). The initial
    /// exchange on connection and reactive pushes still function normally.
    pub fn with_interval(mut self, interval: Option<Duration>) -> Self {
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

    /// Set the metrics purpose label (e.g., "topology", "verifier").
    pub fn with_purpose(mut self, purpose: &'static str) -> Self {
        self.purpose = purpose;
        self
    }
}
