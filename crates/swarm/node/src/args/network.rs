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

/// Default maximum established connections, enforced at the transport layer
/// by the swarm's connection-limits behaviour.
///
/// This is a resource backstop, not a topology-shaping knob: the kademlia
/// topology already budgets its own dials (a 160-peer taper across balanced
/// bins by default) and additionally connects to every available
/// neighborhood peer and accepts a few inbound per bin above target. A
/// healthy saturated table therefore sits in the 200-300 connection range,
/// so the transport cap defaults comfortably above that. Setting it near or
/// below topology's own totals starves the routing table and stalls depth.
const DEFAULT_MAX_PEERS: usize = 400;

/// Default idle timeout in seconds.
const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 60;

/// Default for nat_auto (enabled by default for peer discovery).
fn default_nat_auto() -> bool {
    true
}

/// Default for autonat (AutoNAT v2 enabled by default for all node types).
fn default_autonat() -> bool {
    true
}

/// Default for mdns (local LAN peer discovery enabled by default).
fn default_mdns() -> bool {
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

    /// Stop trusting same-subnet / private-LAN peers, making them ordinary
    /// bin-trim eviction candidates. Local peers are trusted by default.
    #[arg(long = "network.no-trust-local-peers")]
    #[serde(rename = "no_trust_local_peers")]
    pub no_trust_local_peers: bool,

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
    ///
    /// Accepts a bare `--network.nat-auto` (enables) or an explicit
    /// `--network.nat-auto=true`/`--network.nat-auto=false`.
    #[arg(
        long = "network.nat-auto",
        num_args = 0..=1,
        default_value_t = true,
        default_missing_value = "true",
        action = clap::ArgAction::Set,
    )]
    #[serde(default = "default_nat_auto")]
    pub nat_auto: bool,

    /// Enable AutoNAT v2 dial-back reachability verification (enabled by default).
    ///
    /// Accepts a bare `--network.autonat` (enables) or an explicit
    /// `--network.autonat=true`/`--network.autonat=false`.
    #[arg(
        long = "network.autonat",
        num_args = 0..=1,
        default_value_t = true,
        default_missing_value = "true",
        action = clap::ArgAction::Set,
    )]
    #[serde(default = "default_autonat")]
    pub autonat: bool,

    /// Enable UPnP automatic port mapping on the LAN gateway (disabled by default).
    ///
    /// Accepts a bare `--network.upnp` (enables) or an explicit
    /// `--network.upnp=true`/`--network.upnp=false`.
    #[arg(
        long = "network.upnp",
        num_args = 0..=1,
        default_value_t = false,
        default_missing_value = "true",
        action = clap::ArgAction::Set,
    )]
    #[serde(default)]
    pub upnp: bool,

    /// Enable mDNS local LAN peer discovery (enabled by default).
    ///
    /// Accepts a bare `--network.mdns` (enables) or an explicit
    /// `--network.mdns=true`/`--network.mdns=false`.
    #[arg(
        long = "network.mdns",
        num_args = 0..=1,
        default_value_t = true,
        default_missing_value = "true",
        action = clap::ArgAction::Set,
    )]
    #[serde(default = "default_mdns")]
    pub mdns: bool,

    /// Maximum number of established connections (transport-level hard cap).
    ///
    /// Enforced by the swarm independently of kademlia bin logic. Lowering
    /// this below the topology's own connection totals (about 200-300 for a
    /// saturated table) limits routing table health.
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
            no_trust_local_peers: false,
            bootnodes_raw: Vec::new(),
            trusted_peers_raw: Vec::new(),
            port: DEFAULT_P2P_PORT,
            addr: DEFAULT_LISTEN_ADDR.to_string(),
            nat_addrs_raw: Vec::new(),
            nat_auto: true,
            autonat: true,
            upnp: false,
            mdns: true,
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
        if config.bootnodes().is_empty()
            && let Some(spec_bootnodes) = spec.bootnodes()
        {
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
            config.override_bootnodes(parsed?);
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
    autonat: bool,
    upnp: bool,
    mdns: bool,
    discovery_enabled: bool,
    trust_local_peers: bool,
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
            autonat: self.autonat,
            upnp: self.upnp,
            mdns: self.mdns,
            discovery_enabled: self.discovery_enabled,
            trust_local_peers: self.trust_local_peers,
            max_peers: self.max_peers,
            idle_timeout: self.idle_timeout,
            peer: self.peer,
            routing,
        }
    }

    /// Replace the configured bootnodes with a list from a higher-precedence
    /// source (spec defaults when the CLI gave none, or host-supplied
    /// multiaddrs at the FFI boundary).
    pub fn override_bootnodes(&mut self, bootnodes: Vec<Multiaddr>) {
        self.bootnodes = bootnodes;
    }
}

impl Default for NetworkConfig<KademliaConfig> {
    #[allow(clippy::expect_used)]
    fn default() -> Self {
        let listen_addr: Multiaddr =
            format!("/ip4/{}/tcp/{}", DEFAULT_LISTEN_ADDR, DEFAULT_P2P_PORT)
                .parse()
                .expect("default listen address is valid");
        Self {
            listen_addrs: vec![listen_addr],
            bootnodes: Vec::new(),
            trusted_peers: Vec::new(),
            nat_addrs: Vec::new(),
            nat_auto: true,
            autonat: true,
            upnp: false,
            mdns: true,
            discovery_enabled: true,
            trust_local_peers: true,
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
        // The cap is enforced at the transport layer, so zero would deny
        // every connection and isolate the node. Fail fast instead.
        if args.max_peers == 0 {
            return Err(ConfigError::ZeroMaxPeers);
        }

        let listen_addr_str = format!("/ip4/{}/tcp/{}", args.addr, args.port);
        let listen_addrs =
            vec![
                listen_addr_str
                    .parse()
                    .map_err(|e| ConfigError::InvalidAddress {
                        kind: ConfigAddressKind::ListenAddr,
                        addr: listen_addr_str,
                        source: e,
                    })?,
            ];

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
            autonat: args.autonat,
            upnp: args.upnp,
            mdns: args.mdns,
            discovery_enabled: !args.disable_discovery,
            trust_local_peers: !args.no_trust_local_peers,
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

    fn autonat_enabled(&self) -> bool {
        self.autonat
    }

    fn upnp_enabled(&self) -> bool {
        self.upnp
    }

    fn mdns_enabled(&self) -> bool {
        self.mdns
    }

    fn trust_local_peers(&self) -> bool {
        self.trust_local_peers
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
    use vertex_swarm_api::{DEFAULT_PEER_MAX_PER_BIN, PeerConfigValues};

    #[test]
    fn network_config_from_default_args() {
        let args = NetworkArgs::default();
        let config = NetworkConfig::try_from(&args).expect("default args should be valid");

        // Default listen address is constructed from addr:port
        assert!(!config.listen_addrs().is_empty());
        assert_eq!(config.max_peers(), DEFAULT_MAX_PEERS);
        assert_eq!(
            config.idle_timeout(),
            Duration::from_secs(DEFAULT_IDLE_TIMEOUT_SECS)
        );
        assert!(config.discovery_enabled());
    }

    #[test]
    fn nat_traversal_defaults() {
        // AutoNAT v2 is on by default for every node type; UPnP is opt-in.
        let config =
            NetworkConfig::try_from(&NetworkArgs::default()).expect("default args should be valid");
        assert!(config.autonat_enabled());
        assert!(!config.upnp_enabled());
    }

    #[test]
    fn nat_traversal_flags_propagate() {
        let args = NetworkArgs {
            autonat: false,
            upnp: true,
            ..Default::default()
        };
        let config = NetworkConfig::try_from(&args).expect("valid args");
        assert!(!config.autonat_enabled());
        assert!(config.upnp_enabled());
    }

    #[test]
    fn mdns_default_enabled() {
        // mDNS local LAN discovery is on by default for zero-bootnode bootstrap.
        let config =
            NetworkConfig::try_from(&NetworkArgs::default()).expect("default args should be valid");
        assert!(config.mdns_enabled());
    }

    #[derive(clap::Parser)]
    struct TestCli {
        #[command(flatten)]
        network: NetworkArgs,
    }

    #[test]
    fn trust_local_peers_default_enabled() {
        // Local peers are trusted by default and protected from bin trimming.
        let config =
            NetworkConfig::try_from(&NetworkArgs::default()).expect("default args should be valid");
        assert!(config.trust_local_peers());
    }

    #[test]
    fn no_trust_local_peers_flag_flips_default() {
        use clap::Parser;

        #[derive(Parser)]
        struct TestCli {
            #[command(flatten)]
            network: NetworkArgs,
        }

        // Default leaves trust on; the negation flag disables it.
        let default = TestCli::try_parse_from(["test"]).expect("default should parse");
        let config = NetworkConfig::try_from(&default.network).expect("valid args");
        assert!(config.trust_local_peers(), "trust is on by default");

        let parsed = TestCli::try_parse_from(["test", "--network.no-trust-local-peers"])
            .expect("flag should parse");
        let config = NetworkConfig::try_from(&parsed.network).expect("valid args");
        assert!(
            !config.trust_local_peers(),
            "negation flag disables local-peer trust"
        );
    }

    #[test]
    fn mdns_flag_parses() {
        use clap::Parser;

        // The flag is registered under its `network.mdns` long name and parses
        // with no value, leaving mDNS enabled by default.
        let parsed =
            TestCli::try_parse_from(["test", "--network.mdns"]).expect("flag should parse");
        assert!(parsed.network.mdns, "mDNS is enabled by default");

        let default = TestCli::try_parse_from(["test"]).expect("default should parse");
        assert!(default.network.mdns, "mDNS defaults to enabled");
    }

    /// Each boolean networking flag must accept the bare form, an explicit
    /// `=true`, an explicit `=false`, and yield its documented default when
    /// omitted. This guards the CLI usability gap where a default-on flag could
    /// not be disabled from the command line.
    #[test]
    fn boolean_network_flags_accept_explicit_values() {
        use clap::Parser;

        struct Case {
            flag: &'static str,
            default: bool,
            bare: bool,
            get: fn(&NetworkArgs) -> bool,
        }

        let cases = [
            Case {
                flag: "network.mdns",
                default: true,
                bare: true,
                get: |a| a.mdns,
            },
            Case {
                flag: "network.autonat",
                default: true,
                bare: true,
                get: |a| a.autonat,
            },
            Case {
                flag: "network.nat-auto",
                default: true,
                bare: true,
                get: |a| a.nat_auto,
            },
            Case {
                flag: "network.upnp",
                default: false,
                bare: true,
                get: |a| a.upnp,
            },
        ];

        for case in cases {
            let long = format!("--{}", case.flag);

            let omitted = TestCli::try_parse_from(["test"]).expect("default should parse");
            assert_eq!(
                (case.get)(&omitted.network),
                case.default,
                "{} omitted should yield default",
                case.flag
            );

            let bare = TestCli::try_parse_from(["test", &long]).expect("bare form should parse");
            assert_eq!(
                (case.get)(&bare.network),
                case.bare,
                "{} bare form should enable",
                case.flag
            );

            let enabled = TestCli::try_parse_from(["test", &format!("{long}=true")])
                .expect("explicit true should parse");
            assert!(
                (case.get)(&enabled.network),
                "{}=true should enable",
                case.flag
            );

            let disabled = TestCli::try_parse_from(["test", &format!("{long}=false")])
                .expect("explicit false should parse");
            assert!(
                !(case.get)(&disabled.network),
                "{}=false should disable",
                case.flag
            );
        }
    }

    #[test]
    fn mdns_flag_propagates() {
        let args = NetworkArgs {
            mdns: false,
            ..Default::default()
        };
        let config = NetworkConfig::try_from(&args).expect("valid args");
        assert!(!config.mdns_enabled());
    }

    #[test]
    fn network_config_parses_valid_bootnodes() {
        let args = NetworkArgs {
            bootnodes_raw: vec!["/ip4/192.168.1.1/tcp/1634".to_string()],
            ..Default::default()
        };

        let config = NetworkConfig::try_from(&args).expect("valid multiaddrs should parse");

        assert_eq!(config.bootnodes().len(), 1);
    }

    #[test]
    fn network_config_fails_on_invalid_listen_addr() {
        let args = NetworkArgs {
            addr: "not-an-ip".to_string(),
            ..Default::default()
        };

        let result = NetworkConfig::try_from(&args);
        assert!(result.is_err());
    }

    /// The transport enforces max-peers as a hard cap, so a zero value would
    /// isolate the node; configuration validation rejects it up front.
    #[test]
    fn network_config_fails_on_zero_max_peers() {
        let args = NetworkArgs {
            max_peers: 0,
            ..Default::default()
        };

        let result = NetworkConfig::try_from(&args);
        assert!(matches!(result, Err(ConfigError::ZeroMaxPeers)));
    }

    #[test]
    fn network_config_fails_on_invalid_bootnode() {
        let args = NetworkArgs {
            bootnodes_raw: vec!["also-invalid".to_string()],
            ..Default::default()
        };

        let result = NetworkConfig::try_from(&args);
        assert!(result.is_err());
    }

    #[test]
    fn peer_config_from_default_args() {
        let args = PeerArgs::default();
        let config = PeerConfig::from(&args);

        assert_eq!(config.max_per_bin(), DEFAULT_PEER_MAX_PER_BIN);
    }
}
