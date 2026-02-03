//! ClientNode - Swarm node with client protocols for chunk retrieval and upload.
//!
//! A [`ClientNode`] extends the base topology protocols with client protocols:
//! pricing, retrieval, pushsync, and settlement (pseudosettle/swap).
//!
//! Use this for nodes that need to read from and write to the Swarm network.

use std::sync::Arc;

use eyre::Result;
use futures::StreamExt;
use libp2p::{
    Multiaddr, PeerId, SwarmBuilder, identify, identity::PublicKey, noise, swarm::NetworkBehaviour,
    swarm::SwarmEvent, tcp, yamux,
};
use nectar_primitives::SwarmAddress;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use vertex_swarm_api::{SwarmIdentity, SwarmNodeTypes, SwarmTopology};
use vertex_swarm_kademlia::{KademliaConfig, KademliaTopology};
use vertex_swarm_peermanager::{AddressManager, InternalPeerManager, PeerManager, PeerStore};
use vertex_swarm_topology::{TopologyBehaviour, TopologyCommand, TopologyConfig, TopologyEvent};
use vertex_tasks::SpawnableTask;
use vertex_tasks::TaskExecutor;

use super::base::BaseNode;
use super::builder::{BuilderConfig, BuiltInfrastructure};
use crate::protocol::{
    BehaviourConfig as ClientBehaviourConfig, ClientBehaviour, ClientCommand, ClientEvent,
    PseudosettleEvent, SwapEvent,
};
use crate::{ClientHandle, ClientService};

/// Network behaviour for a client node (topology + client protocols).
#[derive(NetworkBehaviour)]
#[behaviour(to_swarm = "ClientNodeEvent")]
pub struct ClientNodeBehaviour<N: SwarmNodeTypes> {
    /// Identify protocol - exchange peer info.
    pub identify: identify::Behaviour,

    /// Topology behaviour - handshake, hive, pingpong.
    pub topology: TopologyBehaviour<N>,

    /// Client behaviour - pricing, retrieval, pushsync, settlement.
    pub client: ClientBehaviour,
}

impl<N: SwarmNodeTypes> ClientNodeBehaviour<N> {
    /// Create a new client node behaviour.
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
            client: ClientBehaviour::new(ClientBehaviourConfig::default()),
        }
    }

    /// Create a new client node behaviour with address management.
    pub fn with_address_manager(
        local_public_key: PublicKey,
        identity: N::Identity,
        peer_manager: Arc<PeerManager>,
        address_manager: Arc<AddressManager>,
    ) -> Self {
        Self {
            identify: identify::Behaviour::new(identify::Config::new(
                "/vertex/1.0.0".to_string(),
                local_public_key,
            )),
            topology: TopologyBehaviour::with_address_manager(
                identity,
                TopologyConfig::default(),
                peer_manager,
                address_manager,
            ),
            client: ClientBehaviour::new(ClientBehaviourConfig::default()),
        }
    }
}

/// Events from the client node behaviour.
pub enum ClientNodeEvent {
    /// Identify protocol event.
    Identify(Box<identify::Event>),
    /// Topology event (peer ready, disconnected, discovered).
    Topology(TopologyEvent),
    /// Client event (pricing, retrieval, pushsync).
    Client(ClientEvent),
}

impl From<identify::Event> for ClientNodeEvent {
    fn from(event: identify::Event) -> Self {
        ClientNodeEvent::Identify(Box::new(event))
    }
}

impl From<TopologyEvent> for ClientNodeEvent {
    fn from(event: TopologyEvent) -> Self {
        ClientNodeEvent::Topology(event)
    }
}

impl From<ClientEvent> for ClientNodeEvent {
    fn from(event: ClientEvent) -> Self {
        ClientNodeEvent::Client(event)
    }
}

/// A Swarm client node with pricing, retrieval, and pushsync protocols.
///
/// Unlike [`BootNode`](super::BootNode), this includes client protocols for
/// reading from and writing to the Swarm network.
///
/// # Example
///
/// ```ignore
/// let (node, service, handle) = ClientNode::<MyTypes>::builder(identity)
///     .with_network_config(&config)
///     .build()
///     .await?;
///
/// // Spawn the service to handle business logic
/// executor.spawn(service.into_task());
///
/// // Run the node
/// node.into_task().await;
/// ```
pub struct ClientNode<N: SwarmNodeTypes> {
    base: BaseNode<N, ClientNodeBehaviour<N>>,

    /// Channel to send events to the client service.
    client_event_tx: mpsc::UnboundedSender<ClientEvent>,

    /// Channel to receive commands from the client service.
    client_command_rx: mpsc::UnboundedReceiver<ClientCommand>,
}

impl<N: SwarmNodeTypes> ClientNode<N> {
    /// Create a builder for constructing a ClientNode.
    pub fn builder(identity: N::Identity) -> ClientNodeBuilder<N> {
        ClientNodeBuilder::new(identity)
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

    /// Dial peers from multiaddr strings.
    ///
    /// Returns the number of successfully initiated dials.
    pub fn dial_addresses(&mut self, addrs: &[String]) -> usize {
        let mut dialed = 0;
        for addr_str in addrs {
            match addr_str.parse::<Multiaddr>() {
                Ok(addr) => {
                    debug!(%addr, "Dialing peer");
                    self.base
                        .swarm
                        .behaviour_mut()
                        .topology
                        .on_command(TopologyCommand::Dial {
                            addr,
                            for_gossip: false,
                        });
                    dialed += 1;
                }
                Err(e) => {
                    warn!(addr = %addr_str, %e, "Invalid multiaddr, skipping");
                }
            }
        }
        dialed
    }

    /// Start listening on configured addresses.
    pub fn start_listening(&mut self) -> Result<()> {
        self.base.start_listening()
    }

    /// Connect to bootnodes.
    pub async fn connect_bootnodes(&mut self) -> Result<usize> {
        self.base.connect_bootnodes().await
    }

    async fn start_and_run(mut self) -> Result<()> {
        self.start_listening()?;
        self.connect_bootnodes().await?;
        self.run().await
    }

    /// Run the network event loop.
    pub async fn run(mut self) -> Result<()> {
        info!("Starting client node event loop");

        // Get reference to Kademlia's dial notify outside the loop
        // to avoid borrow conflicts with &mut self.base.swarm
        let kademlia = self.base.kademlia.clone();

        loop {
            tokio::select! {
                event = self.base.swarm.select_next_some() => {
                    self.handle_swarm_event(event);
                }

                Some(command) = self.client_command_rx.recv() => {
                    self.handle_client_command(command);
                }

                // Kademlia signals when it has dial candidates ready
                _ = kademlia.dial_notify().notified() => {
                    self.base.dial_connection_candidates();
                }
            }
        }
    }

    fn handle_swarm_event(&mut self, event: SwarmEvent<ClientNodeEvent>) {
        if let Some(SwarmEvent::Behaviour(behaviour_event)) =
            self.base.handle_swarm_event_common(event)
        {
            self.handle_behaviour_event(behaviour_event);
        }
    }

    fn handle_behaviour_event(&mut self, event: ClientNodeEvent) {
        match event {
            ClientNodeEvent::Identify(event) => {
                Self::handle_identify_event(*event);
            }
            ClientNodeEvent::Topology(event) => {
                self.handle_topology_event(event);
            }
            ClientNodeEvent::Client(event) => {
                self.route_client_event(event);
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

    fn handle_topology_event(&mut self, event: TopologyEvent) {
        // On peer authentication, activate the client handler
        self.base
            .handle_topology_event(event, |base, overlay, is_full_node| {
                // Resolve peer_id from peer_manager
                if let Some(peer_id) = base.peer_manager.resolve_peer_id(&overlay) {
                    base.swarm
                        .behaviour_mut()
                        .client
                        .on_command(ClientCommand::ActivatePeer {
                            peer_id,
                            overlay,
                            is_full_node,
                        });
                }
            });
    }

    fn route_client_event(&self, event: ClientEvent) {
        if let Err(e) = self.client_event_tx.send(event) {
            warn!(%e, "Failed to send client event to service");
        }
    }

    fn handle_client_command(&mut self, command: ClientCommand) {
        self.base.swarm.behaviour_mut().client.on_command(command);
    }

    /// Get the number of connected peers.
    pub fn connected_peers(&self) -> usize {
        self.base.connected_peers()
    }

    /// Check if we're connected to any peers.
    pub fn is_connected(&self) -> bool {
        self.base.is_connected()
    }
}

impl<N: SwarmNodeTypes> SpawnableTask for ClientNode<N> {
    async fn into_task(self) {
        if let Err(e) = self.start_and_run().await {
            tracing::error!(error = %e, "ClientNode error");
        }
    }
}

/// Builder for ClientNode.
pub struct ClientNodeBuilder<N: SwarmNodeTypes> {
    config: BuilderConfig<N>,
    pseudosettle_event_tx: Option<mpsc::UnboundedSender<PseudosettleEvent>>,
    swap_event_tx: Option<mpsc::UnboundedSender<SwapEvent>>,
}

impl<N: SwarmNodeTypes> ClientNodeBuilder<N> {
    /// Create a new builder.
    pub fn new(identity: N::Identity) -> Self {
        Self {
            config: BuilderConfig::new(identity),
            pseudosettle_event_tx: None,
            swap_event_tx: None,
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

    /// Set the sender for routing pseudosettle events.
    pub fn with_pseudosettle_events(
        mut self,
        tx: mpsc::UnboundedSender<PseudosettleEvent>,
    ) -> Self {
        self.pseudosettle_event_tx = Some(tx);
        self
    }

    /// Set the sender for routing swap events.
    pub fn with_swap_events(mut self, tx: mpsc::UnboundedSender<SwapEvent>) -> Self {
        self.swap_event_tx = Some(tx);
        self
    }

    /// Build the ClientNode and ClientService.
    ///
    /// Returns the node and a client service that should be spawned as a background task.
    pub async fn build(self) -> Result<(ClientNode<N>, ClientService, ClientHandle)> {
        info!("Initializing client P2P network...");

        let infra = BuiltInfrastructure::from_config(self.config)?;

        let identity_for_behaviour = infra.identity.clone();
        let peer_manager_for_behaviour = infra.peer_manager.clone();
        let address_manager_for_behaviour = infra.address_manager.clone();
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
                let behaviour = match address_manager_for_behaviour {
                    Some(mgr) => ClientNodeBehaviour::with_address_manager(
                        keypair.public().clone(),
                        identity_for_behaviour.clone(),
                        peer_manager_for_behaviour.clone(),
                        mgr,
                    ),
                    None => ClientNodeBehaviour::new(
                        keypair.public().clone(),
                        identity_for_behaviour.clone(),
                        peer_manager_for_behaviour.clone(),
                    ),
                };
                Ok(behaviour)
            })?
            .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(idle_timeout))
            .build();

        // Enable gossip with depth provider from kademlia
        let kademlia_for_depth = infra.kademlia.clone();
        swarm.behaviour_mut().topology.enable_gossip(
            vertex_swarm_topology::HiveGossipConfig::default(),
            Arc::new(move || kademlia_for_depth.depth()),
        );

        // Configure settlement event routing
        if let Some(tx) = self.pseudosettle_event_tx {
            swarm.behaviour_mut().client.set_pseudosettle_events(tx);
        }
        if let Some(tx) = self.swap_event_tx {
            swarm.behaviour_mut().client.set_swap_events(tx);
        }

        let local_peer_id = *swarm.local_peer_id();
        info!(%local_peer_id, "Client node peer ID");
        info!(overlay = %infra.identity.overlay_address(), "Overlay address");

        // Spawn stats reporting task
        let executor = TaskExecutor::current();
        let _stats_handle = crate::stats::spawn_stats_task(
            infra.kademlia.clone(),
            crate::stats::StatsConfig::default(),
            &executor,
        );

        // Create channels for client communication
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        // Create the client service
        let (client_service, client_handle) = ClientService::with_channels(command_tx, event_rx);

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

        let node = ClientNode {
            base,
            client_event_tx: event_tx,
            client_command_rx: command_rx,
        };

        Ok((node, client_service, client_handle))
    }
}
