//! Bootnode - minimal Swarm node with topology protocols only.
//!
//! A [`BootNode`] participates in peer discovery via handshake, hive, and pingpong
//! but does not run client protocols (pricing, retrieval, pushsync, settlement).
//!
//! Use this for dedicated bootnode servers that help new nodes join the network.

use eyre::Result;
use futures::StreamExt;
use libp2p::{
    PeerId, SwarmBuilder, identify, identity::PublicKey, noise, swarm::NetworkBehaviour,
    swarm::SwarmEvent, tcp, yamux,
};
use nectar_primitives::SwarmAddress;
use tracing::{debug, info, warn};
use vertex_swarm_api::{SwarmIdentity, SwarmNetworkConfig, SwarmPeerConfig, SwarmRoutingConfig};
use vertex_swarm_topology::{
    KademliaConfig, TopologyBehaviour, TopologyCommand, TopologyConfig, TopologyHandle,
};
use vertex_tasks::SpawnableTask;

use super::base::BaseNode;
use super::builder::{BuiltInfrastructure, TopologyBuildOptions};

/// Network behaviour for a bootnode (topology only, no client protocols).
#[derive(NetworkBehaviour)]
#[behaviour(to_swarm = "BootnodeEvent")]
pub struct BootnodeBehaviour<I: SwarmIdentity> {
    /// Identify protocol - exchange peer info.
    pub identify: identify::Behaviour,

    /// Topology behaviour - handshake, hive, pingpong only.
    pub topology: TopologyBehaviour<I>,
}

impl<I: SwarmIdentity> BootnodeBehaviour<I> {
    /// Create behaviour from pre-built topology (used with libp2p SwarmBuilder).
    pub fn from_parts(local_public_key: PublicKey, topology: TopologyBehaviour<I>) -> Self {
        Self {
            identify: identify::Behaviour::new(identify::Config::new(
                "/vertex/1.0.0".to_string(),
                local_public_key,
            )),
            topology,
        }
    }
}

/// Events from the bootnode behaviour.
pub enum BootnodeEvent {
    /// Identify protocol event.
    Identify(Box<identify::Event>),
    /// Topology events are emitted via TopologyServiceEvent broadcast channel.
    Topology(()),
}

impl From<identify::Event> for BootnodeEvent {
    fn from(event: identify::Event) -> Self {
        BootnodeEvent::Identify(Box::new(event))
    }
}

impl From<()> for BootnodeEvent {
    fn from(_: ()) -> Self {
        BootnodeEvent::Topology(())
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
/// let node = BootNode::builder(identity)
///     .build(&config)
///     .await?;
///
/// node.into_task().await;
/// ```
pub struct BootNode<I: SwarmIdentity> {
    base: BaseNode<I, BootnodeBehaviour<I>>,
}

impl<I: SwarmIdentity> BootNode<I> {
    /// Create a builder for constructing a BootNode.
    pub fn builder(identity: I) -> BootNodeBuilder<I> {
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
    pub fn identity(&self) -> &I {
        self.base.identity()
    }

    /// Get the topology handle for peer and routing queries.
    pub fn topology_handle(&self) -> &TopologyHandle<I> {
        self.base.topology_handle()
    }

    /// Send a topology command.
    pub fn topology_command(&mut self, command: TopologyCommand) {
        self.base.swarm.behaviour_mut().topology.on_command(command);
    }

    /// Start listening on configured addresses.
    pub fn start_listening(&mut self) -> Result<()> {
        self.base.start_listening()
    }

    /// Start listening and run the event loop.
    ///
    /// Bootnode connections are initiated during build().
    async fn start_and_run(mut self) -> Result<()> {
        self.start_listening()?;
        self.run().await
    }

    /// Run the network event loop.
    pub async fn run(mut self) -> Result<()> {
        info!("Starting bootnode event loop");

        loop {
            tokio::select! {
                event = self.base.swarm.next() => {
                    if let Some(event) = event {
                        self.handle_swarm_event(event);
                    }
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
            BootnodeEvent::Topology(_) => {
                // TopologyBehaviour now handles routing updates and emits TopologyServiceEvent
                // directly. Bootnodes don't need to do anything additional.
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

impl<I: SwarmIdentity> SpawnableTask for BootNode<I> {
    async fn into_task(self) {
        if let Err(e) = self.start_and_run().await {
            tracing::error!(error = %e, "BootNode error");
        }
    }
}

/// Builder for BootNode.
pub struct BootNodeBuilder<I: SwarmIdentity> {
    identity: I,
    infra: Option<BuiltInfrastructure<I>>,
    kademlia_config: Option<KademliaConfig>,
    gossip: Option<vertex_swarm_topology::HiveGossipConfig>,
}

impl<I: SwarmIdentity> BootNodeBuilder<I> {
    /// Create a new builder.
    pub fn new(identity: I) -> Self {
        Self {
            identity,
            infra: None,
            kademlia_config: None,
            gossip: Some(vertex_swarm_topology::HiveGossipConfig::default()),
        }
    }

    /// Use pre-built infrastructure (for dependency injection from SwarmNodeBuilder).
    pub fn with_infrastructure(mut self, infra: BuiltInfrastructure<I>) -> Self {
        self.infra = Some(infra);
        self
    }

    /// Set the Kademlia configuration.
    pub fn with_kademlia_config(mut self, kademlia_config: KademliaConfig) -> Self {
        self.kademlia_config = Some(kademlia_config);
        self
    }

    /// Set gossip configuration (None disables gossip).
    pub fn with_gossip(mut self, config: Option<vertex_swarm_topology::HiveGossipConfig>) -> Self {
        self.gossip = config;
        self
    }

    /// Disable gossip-based peer discovery.
    pub fn without_gossip(mut self) -> Self {
        self.gossip = None;
        self
    }
}

impl<I: SwarmIdentity + Clone> BootNodeBuilder<I> {
    /// Build the BootNode using the provided network configuration.
    pub async fn build<C>(self, network_config: &C) -> Result<BootNode<I>>
    where
        C: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig<Routing = KademliaConfig>,
    {
        info!("Initializing bootnode P2P network...");

        let mut infra = match self.infra {
            Some(infra) => infra,
            None => {
                let mut options = TopologyBuildOptions::new();
                if let Some(kademlia) = self.kademlia_config {
                    options = options.with_kademlia(kademlia);
                }
                options = options.with_gossip(self.gossip);
                BuiltInfrastructure::from_config(self.identity, network_config, options)?
            }
        };

        // Extract components for behaviour construction
        let components = infra
            .behaviour_components
            .take()
            .expect("behaviour_components should be present");

        // Build topology behaviour (gossip is auto-enabled via config)
        let (topology_behaviour, _depth_provider) =
            components.into_behaviour(TopologyConfig::default());
        let idle_timeout = infra.idle_timeout;

        // Use Mutex to pass pre-built topology through the closure
        let topology_cell = std::sync::Mutex::new(Some(topology_behaviour));

        let mut swarm = SwarmBuilder::with_new_identity()
            .with_tokio()
            .with_tcp(
                tcp::Config::default(),
                noise::Config::new,
                yamux::Config::default,
            )?
            .with_dns()?
            .with_behaviour(|keypair| {
                let topology = topology_cell
                    .lock()
                    .unwrap()
                    .take()
                    .expect("topology should be present");
                Ok(BootnodeBehaviour::from_parts(keypair.public().clone(), topology))
            })?
            .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(idle_timeout))
            .build();

        let local_peer_id = *swarm.local_peer_id();
        info!(%local_peer_id, "Bootnode peer ID");
        info!(overlay = %infra.identity.overlay_address(), "Overlay address");

        // Connect to bootnodes during build
        let connected = infra.topology_service.connect_bootnodes(|addr| swarm.dial(addr));
        if connected > 0 {
            info!(connected, "Initiated bootnode connections");
        }

        let base = BaseNode {
            swarm,
            identity: infra.identity,
            listen_addrs: infra.listen_addrs,
            topology_handle: infra.topology_handle,
        };

        Ok(BootNode { base })
    }
}
