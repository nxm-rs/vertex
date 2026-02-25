//! P2P network CLI arguments and validated configuration.

use std::time::Duration;

use clap::Args;
use serde::{Deserialize, Serialize};
use vertex_swarm_api::{
    ConfigAddressKind, ConfigError, Multiaddr, SwarmNetworkConfig, SwarmPeerConfig,
    SwarmRoutingConfig,
};
use vertex_swarm_topology::{KademliaConfig, RoutingArgs};

use super::peer::{PeerArgs, PeerConfig};

/// Default P2P listen port.
const DEFAULT_P2P_PORT: u16 = 1634;

/// Default listen address.
const DEFAULT_LISTEN_ADDR: &str = "0.0.0.0";

/// Default maximum peers.
const DEFAULT_MAX_PEERS: usize = 50;

/// Default idle timeout in seconds.
const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 60;

/// Default for nat_auto (enabled by default for peer discovery).
fn default_nat_auto() -> bool {
    true
}

/// P2P network CLI arguments.
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bootnodes_raw: Vec<String>,

    /// Comma-separated list of trusted peer multiaddresses.
    #[arg(long = "network.trusted-peers", value_delimiter = ',')]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trusted_peers_raw: Vec<String>,

    /// P2P listen port.
    #[arg(long = "network.port", default_value_t = DEFAULT_P2P_PORT)]
    pub port: u16,

    /// P2P listen address.
    #[arg(long = "network.addr", default_value = DEFAULT_LISTEN_ADDR)]
    pub addr: String,

    /// External/NAT addresses to advertise.
    #[arg(long = "network.nat-addr", value_delimiter = ',')]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nat_addrs_raw: Vec<String>,

    /// Enable auto-NAT discovery from peer-observed addresses (enabled by default).
    #[arg(long = "network.nat-auto", default_value_t = true)]
    #[serde(default = "default_nat_auto")]
    pub nat_auto: bool,

    /// Maximum number of peers.
    #[arg(long = "network.max-peers", default_value_t = DEFAULT_MAX_PEERS)]
    pub max_peers: usize,

    /// Connection idle timeout in seconds.
    #[arg(long = "network.idle-timeout", default_value_t = DEFAULT_IDLE_TIMEOUT_SECS)]
    pub idle_timeout_secs: u64,

    /// Peer management configuration.
    #[command(flatten)]
    #[serde(default)]
    pub peer: PeerArgs,

    /// Kademlia routing configuration.
    #[command(flatten)]
    #[serde(default)]
    pub routing: RoutingArgs,
}

impl Default for NetworkArgs {
    fn default() -> Self {
        Self {
            disable_discovery: false,
            bootnodes_raw: Vec::new(),
            trusted_peers_raw: Vec::new(),
            port: DEFAULT_P2P_PORT,
            addr: DEFAULT_LISTEN_ADDR.to_string(),
            nat_addrs_raw: Vec::new(),
            nat_auto: true,
            max_peers: DEFAULT_MAX_PEERS,
            idle_timeout_secs: DEFAULT_IDLE_TIMEOUT_SECS,
            peer: PeerArgs::default(),
            routing: RoutingArgs::default(),
        }
    }
}

impl NetworkArgs {
    /// Create validated NetworkConfig from these CLI arguments.
    ///
    /// Uses spec's bootnodes as fallback when no CLI bootnodes are provided.
    pub fn network_config<S: vertex_swarm_api::SwarmSpec>(
        &self,
        spec: &S,
    ) -> Result<NetworkConfig<KademliaConfig>, ConfigError> {
        // Start with base conversion
        let mut config = NetworkConfig::try_from(self)?;

        // If no CLI bootnodes, use spec's default bootnodes
        if config.bootnodes().is_empty() {
            if let Some(spec_bootnodes) = spec.bootnodes() {
                let parsed: Result<Vec<Multiaddr>, _> = spec_bootnodes
                    .iter()
                    .map(|s| {
                        s.parse().map_err(|e| ConfigError::InvalidAddress {
                            kind: ConfigAddressKind::Bootnode,
                            addr: s.clone(),
                            source: e,
                        })
                    })
                    .collect();
                config.set_bootnodes(parsed?);
            }
        }

        Ok(config)
    }
}

/// Validated P2P network configuration, generic over routing type.
///
/// The default routing type is `KademliaConfig`. Use `with_routing()` to
/// transform to a different routing configuration type.
#[derive(Debug, Clone)]
pub struct NetworkConfig<R = KademliaConfig> {
    listen_addrs: Vec<Multiaddr>,
    bootnodes: Vec<Multiaddr>,
    trusted_peers: Vec<Multiaddr>,
    nat_addrs: Vec<Multiaddr>,
    nat_auto: bool,
    discovery_enabled: bool,
    max_peers: usize,
    idle_timeout: Duration,
    peer: PeerConfig,
    routing: R,
}

impl<R> NetworkConfig<R> {
    /// Get the peer configuration.
    pub fn peer(&self) -> &PeerConfig {
        &self.peer
    }

    /// Get the routing configuration.
    pub fn routing(&self) -> &R {
        &self.routing
    }

    /// Replace the routing configuration, changing the type parameter.
    pub fn with_routing<NewR>(self, routing: NewR) -> NetworkConfig<NewR> {
        NetworkConfig {
            listen_addrs: self.listen_addrs,
            bootnodes: self.bootnodes,
            trusted_peers: self.trusted_peers,
            nat_addrs: self.nat_addrs,
            nat_auto: self.nat_auto,
            discovery_enabled: self.discovery_enabled,
            max_peers: self.max_peers,
            idle_timeout: self.idle_timeout,
            peer: self.peer,
            routing,
        }
    }

    /// Set bootnodes (for spec fallback).
    pub fn set_bootnodes(&mut self, bootnodes: Vec<Multiaddr>) {
        self.bootnodes = bootnodes;
    }

    /// Set default peer store path if not already configured via CLI.
    pub fn set_default_peer_store_path(&mut self, path: std::path::PathBuf) {
        self.peer.set_default_store_path(path);
    }
}

impl Default for NetworkConfig<KademliaConfig> {
    fn default() -> Self {
        let listen_addr: Multiaddr = format!("/ip4/{}/tcp/{}", DEFAULT_LISTEN_ADDR, DEFAULT_P2P_PORT)
            .parse()
            .expect("default listen address is valid");
        Self {
            listen_addrs: vec![listen_addr],
            bootnodes: Vec::new(),
            trusted_peers: Vec::new(),
            nat_addrs: Vec::new(),
            nat_auto: true,
            discovery_enabled: true,
            max_peers: DEFAULT_MAX_PEERS,
            idle_timeout: Duration::from_secs(DEFAULT_IDLE_TIMEOUT_SECS),
            peer: PeerConfig::default(),
            routing: KademliaConfig::default(),
        }
    }
}

impl TryFrom<&NetworkArgs> for NetworkConfig<KademliaConfig> {
    type Error = ConfigError;

    fn try_from(args: &NetworkArgs) -> Result<Self, Self::Error> {
        let listen_addr_str = format!("/ip4/{}/tcp/{}", args.addr, args.port);
        let listen_addrs = vec![listen_addr_str
            .parse()
            .map_err(|e| ConfigError::InvalidAddress {
                kind: ConfigAddressKind::ListenAddr,
                addr: listen_addr_str,
                source: e,
            })?];

        let bootnodes = args
            .bootnodes_raw
            .iter()
            .map(|s| {
                s.parse().map_err(|e| ConfigError::InvalidAddress {
                    kind: ConfigAddressKind::Bootnode,
                    addr: s.clone(),
                    source: e,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        let trusted_peers = args
            .trusted_peers_raw
            .iter()
            .map(|s| {
                s.parse().map_err(|e| ConfigError::InvalidAddress {
                    kind: ConfigAddressKind::TrustedPeer,
                    addr: s.clone(),
                    source: e,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        let nat_addrs = args
            .nat_addrs_raw
            .iter()
            .map(|s| {
                s.parse().map_err(|e| ConfigError::InvalidAddress {
                    kind: ConfigAddressKind::NatAddr,
                    addr: s.clone(),
                    source: e,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self {
            listen_addrs,
            bootnodes,
            trusted_peers,
            nat_addrs,
            nat_auto: args.nat_auto,
            discovery_enabled: !args.disable_discovery,
            max_peers: args.max_peers,
            idle_timeout: Duration::from_secs(args.idle_timeout_secs),
            peer: PeerConfig::from(&args.peer),
            routing: args.routing.routing_config(),
        })
    }
}

impl<R> SwarmNetworkConfig for NetworkConfig<R> {
    fn listen_addrs(&self) -> &[Multiaddr] {
        &self.listen_addrs
    }

    fn bootnodes(&self) -> &[Multiaddr] {
        &self.bootnodes
    }

    fn trusted_peers(&self) -> &[Multiaddr] {
        &self.trusted_peers
    }

    fn discovery_enabled(&self) -> bool {
        self.discovery_enabled
    }

    fn max_peers(&self) -> usize {
        self.max_peers
    }

    fn idle_timeout(&self) -> Duration {
        self.idle_timeout
    }

    fn nat_addrs(&self) -> &[Multiaddr] {
        &self.nat_addrs
    }

    fn nat_auto_enabled(&self) -> bool {
        self.nat_auto
    }
}

impl<R> SwarmPeerConfig for NetworkConfig<R> {
    type Peers = PeerConfig;

    fn peers(&self) -> &Self::Peers {
        &self.peer
    }
}

impl<R: Default> SwarmRoutingConfig for NetworkConfig<R> {
    type Routing = R;

    fn routing(&self) -> &R {
        &self.routing
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vertex_swarm_api::PeerConfigValues;

    #[test]
    fn network_config_from_default_args() {
        let args = NetworkArgs::default();
        let config = NetworkConfig::try_from(&args).expect("default args should be valid");

        // Default listen address is constructed from addr:port
        assert!(!config.listen_addrs().is_empty());
        assert_eq!(config.max_peers(), DEFAULT_MAX_PEERS);
        assert_eq!(config.idle_timeout(), Duration::from_secs(DEFAULT_IDLE_TIMEOUT_SECS));
        assert!(config.discovery_enabled());
    }

    #[test]
    fn network_config_parses_valid_bootnodes() {
        let mut args = NetworkArgs::default();
        args.bootnodes_raw = vec!["/ip4/192.168.1.1/tcp/1634".to_string()];

        let config = NetworkConfig::try_from(&args).expect("valid multiaddrs should parse");

        assert_eq!(config.bootnodes().len(), 1);
    }

    #[test]
    fn network_config_fails_on_invalid_listen_addr() {
        let mut args = NetworkArgs::default();
        args.addr = "not-an-ip".to_string();

        let result = NetworkConfig::try_from(&args);
        assert!(result.is_err());
    }

    #[test]
    fn network_config_fails_on_invalid_bootnode() {
        let mut args = NetworkArgs::default();
        args.bootnodes_raw = vec!["also-invalid".to_string()];

        let result = NetworkConfig::try_from(&args);
        assert!(result.is_err());
    }

    #[test]
    fn peer_config_from_default_args() {
        let args = PeerArgs::default();
        let config = PeerConfig::from(&args);

        assert!(config.store_path().is_none());
    }

    #[test]
    fn peer_config_with_store_path() {
        let mut args = PeerArgs::default();
        args.store_path = Some(std::path::PathBuf::from("/tmp/peers.json"));

        let config = PeerConfig::from(&args);

        assert_eq!(
            config.store_path(),
            Some(std::path::PathBuf::from("/tmp/peers.json"))
        );
    }
}
