//! Whole-node hosting through the production transport seam.
//!
//! A vertex node (client or storer shape) enters the sim by handing its
//! builder [`transport_override`] and a [`SimNetworkConfig`]; no private
//! constructors are involved. Two invariants the compiler cannot state:
//!
//! - Node background tasks spawn through the process-global task executor,
//!   which binds the runtime of whichever host installs it first. Give a
//!   world at most one whole-node host per test process, and create its
//!   `TaskManager` inside the host future.
//! - Build the world with [`tokio_io`](crate::SimWorldBuilder::tokio_io):
//!   the topology interface watcher opens OS sockets and needs the real IO
//!   driver.

use std::time::Duration;

use libp2p::{
    Multiaddr, PeerId,
    core::{muxing::StreamMuxerBox, transport::Boxed},
    identity::Keypair,
};

use crate::{SimAuth, noise_stack, plaintext_stack, world::listen_multiaddr};

type NodeTransport = Boxed<(PeerId, StreamMuxerBox)>;
type TransportResult = Result<NodeTransport, Box<dyn std::error::Error + Send + Sync>>;

/// A boxed transport constructor structurally identical to the node
/// builders' transport override, so `with_transport` accepts it directly.
pub fn transport_override(auth: SimAuth) -> Box<dyn FnOnce(&Keypair) -> TransportResult + Send> {
    Box::new(move |keypair: &Keypair| match auth {
        SimAuth::Plaintext => Ok(plaintext_stack(keypair)),
        SimAuth::Noise => noise_stack(keypair).map_err(Into::into),
    })
}

/// Network configuration for a simulated node: listens on the unspecified
/// address (the only bind turmoil accepts) and dials the given bootnodes.
pub struct SimNetworkConfig {
    listen_addrs: Vec<Multiaddr>,
    bootnodes: Vec<Multiaddr>,
    trusted_peers: Vec<Multiaddr>,
    nat_addrs: Vec<Multiaddr>,
    max_peers: usize,
    idle_timeout: Duration,
    peer: SimPeerConfig,
    routing: vertex_swarm_topology::KademliaConfig,
}

impl SimNetworkConfig {
    /// Configuration listening on `port` with the given bootnodes.
    pub fn new(port: u16, bootnodes: Vec<Multiaddr>, max_peers: usize) -> Self {
        Self {
            listen_addrs: vec![listen_multiaddr(port)],
            bootnodes,
            trusted_peers: Vec::new(),
            nat_addrs: Vec::new(),
            max_peers,
            idle_timeout: Duration::from_secs(30),
            peer: SimPeerConfig,
            routing: vertex_swarm_topology::KademliaConfig::default(),
        }
    }

    /// Override the idle-connection timeout.
    ///
    /// Scenario runs hold otherwise-quiet connections across long virtual
    /// horizons, so the timeout must outlive the scenario schedule.
    pub fn with_idle_timeout(mut self, idle_timeout: Duration) -> Self {
        self.idle_timeout = idle_timeout;
        self
    }
}

/// Default peer-config values for a simulated node.
#[derive(Debug, Default)]
pub struct SimPeerConfig;

impl vertex_swarm_api::PeerConfigValues for SimPeerConfig {
    fn ban_threshold(&self) -> f64 {
        vertex_swarm_api::DEFAULT_PEER_BAN_THRESHOLD
    }
}

impl vertex_swarm_api::SwarmNetworkConfig for SimNetworkConfig {
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
        true
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
        false
    }
}

impl vertex_swarm_api::SwarmPeerConfig for SimNetworkConfig {
    type Peers = SimPeerConfig;
    fn peers(&self) -> &Self::Peers {
        &self.peer
    }
}

impl vertex_swarm_api::SwarmRoutingConfig for SimNetworkConfig {
    type Routing = vertex_swarm_topology::KademliaConfig;
    fn routing(&self) -> &Self::Routing {
        &self.routing
    }
}
