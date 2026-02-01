//! P2P network CLI arguments.

use clap::Args;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use vertex_swarm_api::SwarmNetworkConfig;

/// Default P2P listen port.
const DEFAULT_P2P_PORT: u16 = 1634;

/// Default listen address.
const DEFAULT_LISTEN_ADDR: &str = "0.0.0.0";

/// Default maximum peers.
const DEFAULT_MAX_PEERS: usize = 50;

/// Default idle timeout in seconds.
const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 60;

/// P2P network configuration.
#[derive(Debug, Args, Clone, Serialize, Deserialize)]
#[command(next_help_heading = "Networking")]
#[serde(default)]
pub struct NetworkArgs {
    /// Disable the P2P discovery service.
    #[arg(long = "network.no-discovery")]
    #[serde(rename = "no_discovery")]
    pub disable_discovery: bool,

    /// Comma-separated list of bootstrap node multiaddresses.
    #[arg(long = "network.bootnodes", value_delimiter = ',')]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bootnodes: Option<Vec<String>>,

    /// Comma-separated list of trusted peer multiaddresses to connect to on startup.
    ///
    /// Unlike bootnodes, trusted peers are regular nodes that the node will actively
    /// maintain connections with. Useful for connecting to known peers when bootnodes
    /// return no peer addresses (e.g., as a light node connecting to full nodes).
    #[arg(long = "network.trusted-peers", value_delimiter = ',')]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trusted_peers: Option<Vec<String>>,

    /// P2P listen port.
    #[arg(long = "network.port", default_value_t = DEFAULT_P2P_PORT)]
    pub port: u16,

    /// P2P listen address.
    #[arg(long = "network.addr", default_value = DEFAULT_LISTEN_ADDR)]
    pub addr: String,

    /// External/NAT addresses to advertise.
    ///
    /// Comma-separated list of multiaddrs that this node can be reached at from the
    /// public internet. Use when behind NAT or port-forwarding.
    ///
    /// Example: `/ip4/203.0.113.50/tcp/1634,/ip4/203.0.113.50/udp/1634/quic-v1`
    #[arg(long = "network.nat-addr", value_delimiter = ',')]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nat_addrs: Option<Vec<String>>,

    /// Enable auto-NAT discovery from peer-observed addresses.
    ///
    /// When enabled, addresses reported by peers during handshake are used to
    /// infer external addresses after sufficient confirmations.
    #[arg(long = "network.nat-auto")]
    #[serde(default)]
    pub nat_auto: bool,

    /// Maximum number of peers.
    #[arg(long = "network.max-peers", default_value_t = DEFAULT_MAX_PEERS)]
    pub max_peers: usize,

    /// Connection idle timeout in seconds.
    #[arg(long = "network.idle-timeout", default_value_t = DEFAULT_IDLE_TIMEOUT_SECS)]
    pub idle_timeout_secs: u64,
}

impl Default for NetworkArgs {
    fn default() -> Self {
        Self {
            disable_discovery: false,
            bootnodes: None,
            trusted_peers: None,
            port: DEFAULT_P2P_PORT,
            addr: DEFAULT_LISTEN_ADDR.to_string(),
            nat_addrs: None,
            nat_auto: false,
            max_peers: DEFAULT_MAX_PEERS,
            idle_timeout_secs: DEFAULT_IDLE_TIMEOUT_SECS,
        }
    }
}

impl NetworkArgs {
    /// Get the primary listen address as a multiaddr string.
    pub fn listen_multiaddr(&self) -> String {
        format!("/ip4/{}/tcp/{}", self.addr, self.port)
    }
}

impl SwarmNetworkConfig for NetworkArgs {
    fn listen_addrs(&self) -> Vec<String> {
        vec![self.listen_multiaddr()]
    }

    fn bootnodes(&self) -> Vec<String> {
        self.bootnodes.clone().unwrap_or_default()
    }

    fn discovery_enabled(&self) -> bool {
        !self.disable_discovery
    }

    fn max_peers(&self) -> usize {
        self.max_peers
    }

    fn idle_timeout(&self) -> Duration {
        Duration::from_secs(self.idle_timeout_secs)
    }

    fn nat_addrs(&self) -> Vec<String> {
        self.nat_addrs.clone().unwrap_or_default()
    }

    fn nat_auto_enabled(&self) -> bool {
        self.nat_auto
    }
}
