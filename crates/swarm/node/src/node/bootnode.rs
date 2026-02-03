//! Bootnode - minimal Swarm node with topology protocols only.
//!
//! A [`BootNode`] participates in peer discovery via handshake, hive, and pingpong
//! but does not run client protocols (pricing, retrieval, pushsync, settlement).
//!
//! Use this for dedicated bootnode servers that help new nodes join the network.

use std::sync::Arc;

use eyre::Result;
use futures::StreamExt;
use libp2p::{
    Multiaddr, PeerId, SwarmBuilder, identify, identity::PublicKey, noise, swarm::NetworkBehaviour,
    swarm::SwarmEvent, tcp, yamux,
};
use nectar_primitives::SwarmAddress;
use tracing::{debug, info, warn};
use vertex_swarm_api::{SwarmIdentity, SwarmNodeTypes, SwarmTopology};
use vertex_swarm_kademlia::{KademliaConfig, KademliaTopology};
use vertex_swarm_peermanager::{PeerManager, PeerStore};
use vertex_swarm_topology::{TopologyBehaviour, TopologyCommand, TopologyConfig, TopologyEvent};
use vertex_tasks::SpawnableTask;

use super::base::BaseNode;
use super::builder::{BuilderConfig, BuiltInfrastructure};

/// Network behaviour for a bootnode (topology only, no client protocols).
#[derive(NetworkBehaviour)]
#[behaviour(to_swarm = "BootnodeEvent")]
pub struct BootnodeBehaviour<N: SwarmNodeTypes> {
    /// Identify protocol - exchange peer info.
    pub identify: identify::Behaviour,

    /// Topology behaviour - handshake, hive, pingpong only.
    pub topology: TopologyBehaviour<N>,
}

impl<N: SwarmNodeTypes> BootnodeBehaviour<N> {
    /// Create a new bootnode behaviour.
    pub fn new(
        local_public_key: PublicKey,
        identity: N::Identity,
        peer_manager: Arc<PeerManager>,
    ) -> Self {
        Self {
            identify: identify::Behaviour::new(identify::Config::new(
                "/vertex/1.0.0".to_string(),
                local_public_key,
            )),
            topology: TopologyBehaviour::new(identity, TopologyConfig::default(), peer_manager),
        }
    }
}

/// Events from the bootnode behaviour.
pub enum BootnodeEvent {
    /// Identify protocol event.
    Identify(Box<identify::Event>),
    /// Topology event (peer ready, disconnected, discovered).
    Topology(TopologyEvent),
}

impl From<identify::Event> for BootnodeEvent {
    fn from(event: identify::Event) -> Self {
        BootnodeEvent::Identify(Box::new(event))
    }
}

impl From<TopologyEvent> for BootnodeEvent {
    fn from(event: TopologyEvent) -> Self {
        BootnodeEvent::Topology(event)
    }
}

/// A minimal Swarm node with only topology protocols.
///
/// Unlike [`ClientNode`](super::ClientNode), this excludes all client protocols
/// (pricing, retrieval, pushsync, settlement). Bootnodes only participate in
/// peer discovery via handshake, hive, and pingpong.
///
/// # Example
///
/// ```ignore
/// let node = BootNode::<MyTypes>::builder(identity)
///     .with_network_config(&config)
///     .build()
///     .await?;
///
/// node.into_task().await;
/// ```
pub struct BootNode<N: SwarmNodeTypes> {
    base: BaseNode<N, BootnodeBehaviour<N>>,
}

impl<N: SwarmNodeTypes> BootNode<N> {
    /// Create a builder for constructing a BootNode.
    pub fn builder(identity: N::Identity) -> BootNodeBuilder<N> {
        BootNodeBuilder::new(identity)
    }

    /// Get the local peer ID.
    pub fn local_peer_id(&self) -> &PeerId {
        self.base.local_peer_id()
    }

    /// Get the overlay address.
    pub fn overlay_address(&self) -> SwarmAddress {
        self.base.overlay_address()
    }

    /// Get the swarm identity.
    pub fn identity(&self) -> &N::Identity {
        self.base.identity()
    }

    /// Get the peer manager.
    pub fn peer_manager(&self) -> &Arc<PeerManager> {
        self.base.peer_manager()
    }

    /// Get the Kademlia topology.
    pub fn kademlia_topology(&self) -> &Arc<KademliaTopology<N::Identity>> {
        self.base.kademlia_topology()
    }

    /// Send a topology command.
    pub fn topology_command(&mut self, command: TopologyCommand) {
        self.base.swarm.behaviour_mut().topology.on_command(command);
    }

    /// Start listening on configured addresses.
    pub fn start_listening(&mut self) -> Result<()> {
        self.base.start_listening()
    }

    /// Connect to bootnodes.
    pub async fn connect_bootnodes(&mut self) -> Result<usize> {
        self.base.connect_bootnodes().await
    }

    /// Start listening and run the event loop.
    async fn start_and_run(mut self) -> Result<()> {
        self.start_listening()?;
        self.connect_bootnodes().await?;
        self.run().await
    }

    /// Run the network event loop.
    pub async fn run(mut self) -> Result<()> {
        info!("Starting bootnode event loop");

        // Get reference to Kademlia's dial notify outside the loop
        // to avoid borrow conflicts with &mut self.base.swarm
        let kademlia = self.base.kademlia.clone();

        loop {
            tokio::select! {
                event = self.base.swarm.next() => {
                    if let Some(event) = event {
                        self.handle_swarm_event(event);
                    }
                }

                // Kademlia signals when it has dial candidates ready
                _ = kademlia.dial_notify().notified() => {
                    self.base.dial_connection_candidates();
                }
            }
        }
    }

    fn handle_swarm_event(&mut self, event: SwarmEvent<BootnodeEvent>) {
        if let Some(SwarmEvent::Behaviour(behaviour_event)) =
            self.base.handle_swarm_event_common(event)
        {
            self.handle_behaviour_event(behaviour_event);
        }
    }

    fn handle_behaviour_event(&mut self, event: BootnodeEvent) {
        match event {
            BootnodeEvent::Identify(boxed_event) => {
                Self::handle_identify_event(*boxed_event);
            }
            BootnodeEvent::Topology(event) => {
                // Bootnodes don't activate any client handler on peer auth
                self.base.handle_topology_event(event, |_, _, _| {});
            }
        }
    }

    fn handle_identify_event(event: identify::Event) {
        match event {
            identify::Event::Received { peer_id, info, .. } => {
                debug!(
                    %peer_id,
                    protocol_version = %info.protocol_version,
                    agent_version = %info.agent_version,
                    "Received identify info"
                );
            }
            identify::Event::Sent { peer_id, .. } => {
                debug!(%peer_id, "Sent identify info");
            }
            identify::Event::Pushed { peer_id, .. } => {
                debug!(%peer_id, "Pushed identify info");
            }
            identify::Event::Error { peer_id, error, .. } => {
                warn!(%peer_id, %error, "Identify error");
            }
        }
    }

    /// Get the number of connected peers.
    pub fn connected_peers(&self) -> usize {
        self.base.connected_peers()
    }
}

impl<N: SwarmNodeTypes> SpawnableTask for BootNode<N> {
    async fn into_task(self) {
        if let Err(e) = self.start_and_run().await {
            tracing::error!(error = %e, "BootNode error");
        }
    }
}

/// Builder for BootNode.
pub struct BootNodeBuilder<N: SwarmNodeTypes> {
    config: BuilderConfig<N>,
}

impl<N: SwarmNodeTypes> BootNodeBuilder<N> {
    /// Create a new builder.
    pub fn new(identity: N::Identity) -> Self {
        Self {
            config: BuilderConfig::new(identity),
        }
    }

    /// Set network configuration.
    pub fn with_network_config(
        mut self,
        config: &impl vertex_swarm_api::SwarmNetworkConfig,
    ) -> Self {
        self.config.apply_network_config(config);
        self
    }

    /// Set the bootnodes.
    pub fn with_bootnodes(mut self, bootnodes: Vec<Multiaddr>) -> Self {
        self.config.bootnodes = bootnodes;
        self
    }

    /// Set the listen addresses.
    pub fn with_listen_addrs(mut self, addrs: Vec<Multiaddr>) -> Self {
        self.config.listen_addrs = addrs;
        self
    }

    /// Set the Kademlia configuration.
    pub fn with_kademlia_config(mut self, kademlia_config: KademliaConfig) -> Self {
        self.config.kademlia_config = kademlia_config;
        self
    }

    /// Set the peer store.
    pub fn with_peer_store(mut self, store: Arc<PeerStore>) -> Self {
        self.config.peer_store = Some(store);
        self
    }

    /// Build the BootNode.
    pub async fn build(self) -> Result<BootNode<N>> {
        info!("Initializing bootnode P2P network...");

        let infra = BuiltInfrastructure::from_config(self.config)?;

        let identity_for_behaviour = infra.identity.clone();
        let peer_manager_for_behaviour = infra.peer_manager.clone();
        let idle_timeout = infra.idle_timeout;

        let mut swarm = SwarmBuilder::with_new_identity()
            .with_tokio()
            .with_tcp(
                tcp::Config::default(),
                noise::Config::new,
                yamux::Config::default,
            )?
            .with_dns()?
            .with_behaviour(|keypair| {
                Ok(BootnodeBehaviour::new(
                    keypair.public().clone(),
                    identity_for_behaviour.clone(),
                    peer_manager_for_behaviour.clone(),
                ))
            })?
            .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(idle_timeout))
            .build();

        // Enable gossip with depth provider from kademlia
        let kademlia_for_depth = infra.kademlia.clone();
        swarm.behaviour_mut().topology.enable_gossip(
            vertex_swarm_topology::HiveGossipConfig::default(),
            Arc::new(move || kademlia_for_depth.depth()),
        );

        let local_peer_id = *swarm.local_peer_id();
        info!(%local_peer_id, "Bootnode peer ID");
        info!(overlay = %infra.identity.overlay_address(), "Overlay address");

        let base = BaseNode {
            swarm,
            identity: infra.identity,
            peer_manager: infra.peer_manager,
            address_manager: infra.address_manager,
            kademlia: infra.kademlia,
            bootnode_connector: infra.bootnode_connector,
            listen_addrs: infra.listen_addrs,
            discovery_tx: infra.discovery_tx,
        };

        Ok(BootNode { base })
    }
}
