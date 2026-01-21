//! P2P networking for the Vertex Swarm node.
//!
//! This module handles:
//! - Creating the libp2p Swarm with appropriate transports and behaviours
//! - Connecting to bootnodes
//! - Managing peer connections
//! - Performing the Swarm handshake to exchange overlay addresses
//!
//! # Transport Stack
//!
//! The transport stack is:
//! ```text
//! DNS (resolves /dnsaddr/, /dns/, /dns4/, /dns6/)
//!   └── TCP
//!         └── Noise (encryption)
//!               └── Yamux (multiplexing)
//! ```
//!
//! # Behaviours
//!
//! The swarm combines multiple behaviours:
//! - Identify: Exchange peer information
//! - Ping: Keep connections alive
//! - Handshake: Swarm-specific overlay address exchange

use crate::identity::NodeIdentity;
use eyre::Result;
use futures::StreamExt;
use libp2p::{
    identify,
    identity::PublicKey,
    noise, ping,
    swarm::{NetworkBehaviour, SwarmEvent},
    tcp, yamux, Multiaddr, PeerId, Swarm, SwarmBuilder,
};
use std::{sync::Arc, time::Duration};
use tracing::{debug, info, warn};
use vertex_net_handshake::{HandshakeBehaviour, HandshakeEvent, HandshakeInfo};
use vertex_net_primitives_traits::NodeAddress as NodeAddressTrait;
use vertex_net_topology::BootnodeConnector;

/// Configuration for the network layer.
#[derive(Debug, Clone)]
pub struct NetworkConfig {
    /// Listen addresses for incoming connections.
    pub listen_addrs: Vec<Multiaddr>,

    /// Bootnodes to connect to on startup.
    pub bootnodes: Vec<Multiaddr>,

    /// Connection idle timeout.
    pub idle_timeout: Duration,

    /// Maximum number of established connections.
    pub max_connections: u32,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            listen_addrs: vec![
                "/ip4/0.0.0.0/tcp/1634".parse().unwrap(),
                "/ip6/::/tcp/1634".parse().unwrap(),
            ],
            bootnodes: vec![],
            idle_timeout: Duration::from_secs(30),
            max_connections: 50,
        }
    }
}

impl NetworkConfig {
    /// Create a config for mainnet with default bootnodes.
    pub fn mainnet() -> Self {
        Self {
            bootnodes: vec!["/dnsaddr/mainnet.ethswarm.org".parse().unwrap()],
            ..Default::default()
        }
    }

    /// Create a config for testnet with default bootnodes.
    pub fn testnet() -> Self {
        Self {
            bootnodes: vec!["/dnsaddr/testnet.ethswarm.org".parse().unwrap()],
            ..Default::default()
        }
    }
}

/// Combined network behaviour for the Swarm node.
#[derive(NetworkBehaviour)]
pub struct VertexBehaviour {
    /// Identify protocol - exchange peer info.
    identify: identify::Behaviour,

    /// Ping protocol - keep connections alive.
    ping: ping::Behaviour,

    /// Swarm handshake protocol - exchange overlay addresses.
    handshake: HandshakeBehaviour<NodeIdentity>,
}

impl VertexBehaviour {
    /// Create a new behaviour with the given local public key and identity.
    pub fn new(local_public_key: PublicKey, identity: Arc<NodeIdentity>) -> Self {
        Self {
            identify: identify::Behaviour::new(identify::Config::new(
                "/vertex/1.0.0".to_string(),
                local_public_key,
            )),
            ping: ping::Behaviour::new(ping::Config::new().with_interval(Duration::from_secs(15))),
            handshake: HandshakeBehaviour::new(identity),
        }
    }
}

/// The network handle for managing the libp2p swarm.
pub struct Network {
    /// The libp2p swarm.
    swarm: Swarm<VertexBehaviour>,

    /// Bootnode connector.
    bootnode_connector: BootnodeConnector,

    /// Network configuration.
    config: NetworkConfig,

    /// Node identity.
    identity: Arc<NodeIdentity>,
}

impl Network {
    /// Create a new network instance.
    pub async fn new(config: NetworkConfig, identity: NodeIdentity) -> Result<Self> {
        info!("Initializing P2P network...");

        let identity = Arc::new(identity);
        let identity_for_behaviour = identity.clone();

        // Build the swarm with DNS-enabled transport
        let swarm = SwarmBuilder::with_new_identity()
            .with_tokio()
            .with_tcp(
                tcp::Config::default(),
                noise::Config::new,
                yamux::Config::default,
            )?
            .with_dns()?
            .with_behaviour(|keypair| {
                Ok(VertexBehaviour::new(
                    keypair.public().clone(),
                    identity_for_behaviour.clone(),
                ))
            })?
            .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(config.idle_timeout))
            .build();

        let local_peer_id = *swarm.local_peer_id();
        info!(%local_peer_id, "Local peer ID");

        let bootnode_connector = BootnodeConnector::new(config.bootnodes.clone());

        Ok(Self {
            swarm,
            bootnode_connector,
            config,
            identity,
        })
    }

    /// Get the local peer ID.
    pub fn local_peer_id(&self) -> &PeerId {
        self.swarm.local_peer_id()
    }

    /// Start listening on configured addresses.
    pub fn start_listening(&mut self) -> Result<()> {
        for addr in &self.config.listen_addrs {
            match self.swarm.listen_on(addr.clone()) {
                Ok(_) => info!(%addr, "Listening on address"),
                Err(e) => warn!(%addr, %e, "Failed to listen on address"),
            }
        }
        Ok(())
    }

    /// Connect to bootnodes.
    ///
    /// This dials the configured bootnodes. DNS addresses like `/dnsaddr/mainnet.ethswarm.org`
    /// are resolved automatically by libp2p's DNS transport.
    pub async fn connect_bootnodes(&mut self) -> Result<usize> {
        let bootnodes = self.bootnode_connector.shuffled_bootnodes();

        if bootnodes.is_empty() {
            warn!("No bootnodes configured");
            return Ok(0);
        }

        info!(count = bootnodes.len(), "Connecting to bootnodes...");

        let mut connected = 0;
        let min_connections = self.bootnode_connector.min_connections();

        for bootnode in bootnodes {
            if connected >= min_connections {
                info!(connected, "Reached minimum bootnode connections");
                break;
            }

            let is_dns = BootnodeConnector::is_dns_addr(&bootnode);
            info!(
                %bootnode,
                is_dns,
                "Dialing bootnode{}",
                if is_dns { " (DNS will be resolved by libp2p)" } else { "" }
            );

            match self.swarm.dial(bootnode.clone()) {
                Ok(_) => {
                    debug!(%bootnode, "Dial initiated");
                    // Note: This just initiates the dial, we need to wait for connection
                    // in the event loop to confirm success
                    connected += 1;
                }
                Err(e) => {
                    warn!(%bootnode, %e, "Failed to dial bootnode");
                }
            }
        }

        Ok(connected)
    }

    /// Run the network event loop.
    ///
    /// This processes swarm events and should be run in a background task.
    pub async fn run(&mut self) -> Result<()> {
        info!("Starting network event loop");

        loop {
            match self.swarm.select_next_some().await {
                SwarmEvent::NewListenAddr { address, .. } => {
                    info!(%address, "New listen address");
                }
                SwarmEvent::ConnectionEstablished {
                    peer_id,
                    endpoint,
                    num_established,
                    ..
                } => {
                    info!(
                        %peer_id,
                        endpoint = %endpoint.get_remote_address(),
                        num_established,
                        "Connection established"
                    );
                }
                SwarmEvent::ConnectionClosed {
                    peer_id,
                    cause,
                    num_established,
                    ..
                } => {
                    info!(
                        %peer_id,
                        num_established,
                        cause = ?cause,
                        "Connection closed"
                    );
                }
                SwarmEvent::IncomingConnection {
                    local_addr,
                    send_back_addr,
                    ..
                } => {
                    debug!(%local_addr, %send_back_addr, "Incoming connection");
                }
                SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
                    if let Some(peer_id) = peer_id {
                        warn!(%peer_id, %error, "Outgoing connection error");
                    } else {
                        warn!(%error, "Outgoing connection error (unknown peer)");
                    }
                }
                SwarmEvent::Behaviour(event) => {
                    self.handle_behaviour_event(event);
                }
                _ => {}
            }
        }
    }

    /// Handle behaviour-specific events.
    fn handle_behaviour_event(&mut self, event: VertexBehaviourEvent) {
        match event {
            VertexBehaviourEvent::Identify(identify::Event::Received { peer_id, info, .. }) => {
                info!(
                    %peer_id,
                    protocol_version = %info.protocol_version,
                    agent_version = %info.agent_version,
                    listen_addrs = ?info.listen_addrs,
                    "Received identify info"
                );
                // Log protocols separately for clarity
                debug!(%peer_id, "Peer supported protocols:");
                for protocol in &info.protocols {
                    debug!(%peer_id, protocol = %protocol, "  Protocol");
                }
            }
            VertexBehaviourEvent::Identify(identify::Event::Sent { peer_id, .. }) => {
                debug!(%peer_id, "Sent identify info");
            }
            VertexBehaviourEvent::Identify(identify::Event::Pushed { peer_id, .. }) => {
                debug!(%peer_id, "Pushed identify info");
            }
            VertexBehaviourEvent::Identify(identify::Event::Error { peer_id, error, .. }) => {
                warn!(%peer_id, %error, "Identify error");
            }
            VertexBehaviourEvent::Ping(ping::Event { peer, result, .. }) => match result {
                Ok(rtt) => {
                    debug!(%peer, ?rtt, "Ping success");
                }
                Err(e) => {
                    warn!(%peer, %e, "Ping failed");
                }
            },
            VertexBehaviourEvent::Handshake(event) => {
                self.handle_handshake_event(event);
            }
        }
    }

    /// Handle handshake protocol events.
    fn handle_handshake_event(&mut self, event: HandshakeEvent) {
        match event {
            HandshakeEvent::Completed(info) => {
                let HandshakeInfo { peer_id, ack } = info;
                info!(
                    %peer_id,
                    overlay = %hex::encode(ack.node_address().overlay_address().as_slice()),
                    is_full_node = ack.full_node(),
                    welcome_message = %ack.welcome_message(),
                    "Handshake completed"
                );
            }
            HandshakeEvent::Failed(error) => {
                warn!(%error, "Handshake failed");
            }
        }
    }

    /// Get the number of connected peers.
    pub fn connected_peers(&self) -> usize {
        self.swarm.connected_peers().count()
    }

    /// Check if we're connected to any peers.
    pub fn is_connected(&self) -> bool {
        self.connected_peers() > 0
    }
}

