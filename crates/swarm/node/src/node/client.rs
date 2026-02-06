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
use vertex_swarm_api::{SwarmIdentity, SwarmNetworkConfig, SwarmPeerConfig, SwarmRoutingConfig};
use vertex_swarm_topology::{
    KademliaConfig, TopologyBehaviour, TopologyCommand, TopologyHandle, TopologyServiceEvent,
};
use vertex_tasks::SpawnableTask;
use vertex_tasks::TaskExecutor;

use super::base::BaseNode;
use super::builder::{BuiltInfrastructure, TopologyBuildOptions};
use crate::protocol::{
    BehaviourConfig as ClientBehaviourConfig, ClientBehaviour, ClientCommand, ClientEvent,
    PseudosettleEvent, SwapEvent,
};
use crate::{ClientHandle, ClientService};

/// Network behaviour for a client node (topology + client protocols).
#[derive(NetworkBehaviour)]
#[behaviour(to_swarm = "ClientNodeEvent")]
pub struct ClientNodeBehaviour<I: SwarmIdentity + Clone> {
    pub identify: identify::Behaviour,
    pub topology: TopologyBehaviour<I>,
    pub client: ClientBehaviour,
}

impl<I: SwarmIdentity + Clone> ClientNodeBehaviour<I> {
    pub fn from_parts(local_public_key: PublicKey, topology: TopologyBehaviour<I>) -> Self {
        Self {
            identify: identify::Behaviour::new(identify::Config::new(
                "/vertex/1.0.0".to_string(),
                local_public_key,
            )),
            topology,
            client: ClientBehaviour::new(ClientBehaviourConfig::default()),
        }
    }
}

/// Events from the client node behaviour.
pub enum ClientNodeEvent {
    Identify(Box<identify::Event>),
    Topology(()),
    Client(ClientEvent),
}

impl From<identify::Event> for ClientNodeEvent {
    fn from(event: identify::Event) -> Self {
        ClientNodeEvent::Identify(Box::new(event))
    }
}

impl From<()> for ClientNodeEvent {
    fn from(_: ()) -> Self {
        ClientNodeEvent::Topology(())
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
pub struct ClientNode<I: SwarmIdentity + Clone> {
    base: BaseNode<I, ClientNodeBehaviour<I>>,
    client_event_tx: mpsc::UnboundedSender<ClientEvent>,
    client_command_rx: mpsc::UnboundedReceiver<ClientCommand>,
}

impl<I: SwarmIdentity + Clone> ClientNode<I> {
    pub fn builder(identity: I) -> ClientNodeBuilder<I> {
        ClientNodeBuilder::new(identity)
    }

    pub fn local_peer_id(&self) -> &PeerId {
        self.base.local_peer_id()
    }

    pub fn overlay_address(&self) -> SwarmAddress {
        self.base.overlay_address()
    }

    pub fn identity(&self) -> &I {
        self.base.identity()
    }

    pub fn topology_handle(&self) -> &TopologyHandle<I> {
        self.base.topology_handle()
    }

    pub fn topology_command(&mut self, command: TopologyCommand) {
        self.base.swarm.behaviour_mut().topology.on_command(command);
    }

    /// Dial peers from multiaddr strings. Returns the number of successfully initiated dials.
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

    pub fn start_listening(&mut self) -> Result<()> {
        self.base.start_listening()
    }

    async fn start_and_run(mut self) -> Result<()> {
        self.start_listening()?;
        self.run().await
    }

    pub async fn run(mut self) -> Result<()> {
        info!("Starting client node event loop");

        let mut topo_events = self.base.topology_handle.subscribe();

        loop {
            tokio::select! {
                event = self.base.swarm.select_next_some() => {
                    self.handle_swarm_event(event);
                }

                Some(command) = self.client_command_rx.recv() => {
                    self.handle_client_command(command);
                }

                result = topo_events.recv() => {
                    if let Ok(event) = result {
                        self.handle_topology_service_event(event);
                    }
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
            ClientNodeEvent::Topology(_) => {}
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

    fn handle_topology_service_event(&mut self, event: TopologyServiceEvent) {
        match event {
            TopologyServiceEvent::PeerReady {
                overlay,
                peer_id,
                is_full_node,
            } => {
                self.base
                    .swarm
                    .behaviour_mut()
                    .client
                    .on_command(ClientCommand::ActivatePeer {
                        peer_id,
                        overlay,
                        is_full_node,
                    });
            }
            TopologyServiceEvent::PeerDisconnected { .. } => {}
            TopologyServiceEvent::DepthChanged { .. } => {}
            TopologyServiceEvent::DialFailed { .. } => {}
        }
    }

    fn route_client_event(&self, event: ClientEvent) {
        if let Err(e) = self.client_event_tx.send(event) {
            warn!(%e, "Failed to send client event to service");
        }
    }

    fn handle_client_command(&mut self, command: ClientCommand) {
        self.base.swarm.behaviour_mut().client.on_command(command);
    }

    pub fn connected_peers(&self) -> usize {
        self.base.connected_peers()
    }

    pub fn is_connected(&self) -> bool {
        self.base.is_connected()
    }
}

impl<I: SwarmIdentity + Clone> SpawnableTask for ClientNode<I> {
    async fn into_task(self) {
        if let Err(e) = self.start_and_run().await {
            tracing::error!(error = %e, "ClientNode error");
        }
    }
}

/// Builder for ClientNode.
pub struct ClientNodeBuilder<I: SwarmIdentity> {
    identity: I,
    infra: Option<BuiltInfrastructure<I>>,
    kademlia_config: Option<KademliaConfig>,
    pseudosettle_event_tx: Option<mpsc::UnboundedSender<PseudosettleEvent>>,
    swap_event_tx: Option<mpsc::UnboundedSender<SwapEvent>>,
}

impl<I: SwarmIdentity> ClientNodeBuilder<I> {
    pub fn new(identity: I) -> Self {
        Self {
            identity,
            infra: None,
            kademlia_config: None,
            pseudosettle_event_tx: None,
            swap_event_tx: None,
        }
    }

    pub fn with_infrastructure(mut self, infra: BuiltInfrastructure<I>) -> Self {
        self.infra = Some(infra);
        self
    }

    pub fn with_kademlia_config(mut self, kademlia_config: KademliaConfig) -> Self {
        self.kademlia_config = Some(kademlia_config);
        self
    }

    pub fn with_pseudosettle_events(
        mut self,
        tx: mpsc::UnboundedSender<PseudosettleEvent>,
    ) -> Self {
        self.pseudosettle_event_tx = Some(tx);
        self
    }

    pub fn with_swap_events(mut self, tx: mpsc::UnboundedSender<SwapEvent>) -> Self {
        self.swap_event_tx = Some(tx);
        self
    }
}

impl<I: SwarmIdentity + Clone> ClientNodeBuilder<I> {
    pub async fn build<C>(
        self,
        network_config: &C,
    ) -> Result<(ClientNode<I>, ClientService, ClientHandle)>
    where
        C: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig<Routing = KademliaConfig>,
    {
        info!("Initializing client P2P network...");

        let mut infra = match self.infra {
            Some(infra) => infra,
            None => {
                let mut options = TopologyBuildOptions::new();
                if let Some(kademlia) = self.kademlia_config {
                    options = options.with_kademlia(kademlia);
                }
                BuiltInfrastructure::from_config(self.identity, network_config, options)?
            }
        };

        let topology_behaviour = infra
            .take_behaviour()
            .expect("topology_behaviour should be present");
        let idle_timeout = network_config.idle_timeout();
        let listen_addrs = network_config.listen_addrs().to_vec();

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
                Ok(ClientNodeBehaviour::from_parts(keypair.public().clone(), topology))
            })?
            .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(idle_timeout))
            .build();

        if let Some(tx) = self.pseudosettle_event_tx {
            swarm.behaviour_mut().client.set_pseudosettle_events(tx);
        }
        if let Some(tx) = self.swap_event_tx {
            swarm.behaviour_mut().client.set_swap_events(tx);
        }

        let local_peer_id = *swarm.local_peer_id();
        info!(%local_peer_id, "Client node peer ID");
        info!(overlay = %infra.identity.overlay_address(), "Overlay address");

        if infra.topology_handle.connect_bootnodes().await.is_err() {
            warn!("Failed to send connect_bootnodes command");
        }

        let executor = TaskExecutor::current();
        let _stats_handle = crate::stats::spawn_stats_task(
            Arc::new(infra.topology_handle.clone()),
            crate::stats::StatsConfig::default(),
            &executor,
        );

        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        let (client_service, client_handle) = ClientService::with_channels(command_tx, event_rx);

        let base = BaseNode {
            swarm,
            identity: infra.identity,
            listen_addrs,
            topology_handle: infra.topology_handle,
        };

        let node = ClientNode {
            base,
            client_event_tx: event_tx,
            client_command_rx: command_rx,
        };

        Ok((node, client_service, client_handle))
    }
}
