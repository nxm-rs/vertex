//! ClientNode - Swarm node with client protocols for chunk retrieval and upload.
//!
//! A [`ClientNode`] extends the base topology protocols with client protocols:
//! pricing, retrieval, pushsync, and settlement (pseudosettle/swap).
//!
//! Use this for nodes that need to read from and write to the Swarm network.

use std::convert::Infallible;
use std::sync::Arc;

use eyre::Result;
use futures::StreamExt;
#[cfg(not(target_arch = "wasm32"))]
use libp2p::autonat::v2 as autonat;
use libp2p::connection_limits;
#[cfg(not(target_arch = "wasm32"))]
use libp2p::mdns;
#[cfg(not(target_arch = "wasm32"))]
use libp2p::swarm::behaviour::toggle::Toggle;
#[cfg(not(target_arch = "wasm32"))]
use libp2p::upnp;
use libp2p::{Multiaddr, PeerId, identity::PublicKey, swarm::NetworkBehaviour, swarm::SwarmEvent};
use nectar_primitives::SwarmAddress;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use vertex_swarm_api::{SwarmIdentity, SwarmNetworkConfig, SwarmPeerConfig, SwarmRoutingConfig};
use vertex_swarm_net_identify as identify;
use vertex_swarm_topology::{
    KademliaConfig, TopologyBehaviour, TopologyCommand, TopologyConfig, TopologyEvent,
    TopologyHandle,
};
use vertex_tasks::GracefulShutdown;
use vertex_tasks::TaskExecutor;

use super::base::BaseNode;
#[cfg(not(target_arch = "wasm32"))]
use super::base::NatBehaviours;
use super::builder::BuiltInfrastructure;
use crate::protocol::{
    BehaviourConfig as ClientBehaviourConfig, ClientBehaviour, ClientCommand, ClientEvent,
    PseudosettleEvent,
};
use crate::{ClientHandle, ClientService};

/// Network behaviour for a client node (topology + client protocols).
#[derive(NetworkBehaviour)]
#[behaviour(to_swarm = "ClientNodeEvent")]
pub(crate) struct ClientNodeBehaviour<I: SwarmIdentity + Clone> {
    /// Transport-level connection caps (total, per-peer, pending). Listed
    /// first so a denied connection is rejected before the other behaviours
    /// allocate per-connection state.
    pub(crate) connection_limits: connection_limits::Behaviour,
    pub(crate) identify: identify::Behaviour,
    // NAT traversal (AutoNAT v2, UPnP) and LAN discovery (mDNS) are native-only.
    // The browser client dials over websockets, never listens, and the wasm
    // dependency set drops the `autonat`, `upnp`, and `mdns` libp2p features, so
    // these fields are absent from the wasm composite.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) autonat_client: Toggle<autonat::client::Behaviour>,
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) autonat_server: Toggle<autonat::server::Behaviour>,
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) upnp: Toggle<upnp::tokio::Behaviour>,
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) mdns: Toggle<mdns::tokio::Behaviour>,
    pub(crate) topology: TopologyBehaviour<I>,
    pub(crate) client: ClientBehaviour,
}

#[cfg(not(target_arch = "wasm32"))]
impl<I: SwarmIdentity + Clone> ClientNodeBehaviour<I> {
    pub(crate) fn from_parts(
        local_public_key: PublicKey,
        topology: TopologyBehaviour<I>,
        nat: NatBehaviours,
        connection_limits: connection_limits::Behaviour,
    ) -> Self {
        let agent_versions = topology.agent_versions();
        let peer_id = local_public_key.to_peer_id();
        Self {
            connection_limits,
            // Identify advertises addresses scoped to each peer (see
            // `addresses_for_remote`), so a public peer never receives our
            // private or loopback addresses. A NAT'd node with no public address
            // sends an empty listen set to public peers; the reference peer
            // tolerates this (it falls back to the connection's remote
            // multiaddr).
            identify: identify::Behaviour::new(
                identify::Config::new(local_public_key),
                agent_versions,
            ),
            autonat_client: nat.autonat_client,
            autonat_server: nat.autonat_server,
            upnp: nat.upnp,
            mdns: super::base::build_mdns_toggle(nat.mdns_enabled, peer_id),
            topology,
            client: ClientBehaviour::new(ClientBehaviourConfig::default()),
        }
    }
}

/// The browser client composite has no NAT-traversal behaviours: it dials over
/// websockets, never listens, and the `autonat`, `upnp`, and `mdns` libp2p
/// features are dropped from the wasm dependency set.
#[cfg(target_arch = "wasm32")]
impl<I: SwarmIdentity + Clone> ClientNodeBehaviour<I> {
    pub(crate) fn from_parts(
        local_public_key: PublicKey,
        topology: TopologyBehaviour<I>,
        connection_limits: connection_limits::Behaviour,
    ) -> Self {
        let agent_versions = topology.agent_versions();
        Self {
            connection_limits,
            identify: identify::Behaviour::new(
                identify::Config::new(local_public_key),
                agent_versions,
            ),
            topology,
            client: ClientBehaviour::new(ClientBehaviourConfig::default()),
        }
    }
}

/// Assemble the client base node natively, wiring the NAT-traversal behaviours
/// (AutoNAT v2, UPnP) and LAN discovery (mDNS) into the composite.
#[cfg(not(target_arch = "wasm32"))]
async fn build_client_base<I, C>(
    infra: BuiltInfrastructure<I>,
    network_config: &C,
) -> Result<BaseNode<I, ClientNodeBehaviour<I>>>
where
    I: SwarmIdentity + Clone,
    C: SwarmNetworkConfig,
{
    let nat = NatBehaviours::from_config(network_config);
    let connection_limits = super::base::build_connection_limits(network_config);
    super::builder::build_base_node(infra, network_config, "Client node", move |pk, topology| {
        ClientNodeBehaviour::from_parts(pk, topology, nat, connection_limits)
    })
    .await
}

/// Assemble the browser client base node. The wasm composite carries only the
/// connection-limits gate, identify, topology, and the client protocols; the
/// NAT-traversal behaviours are absent.
#[cfg(target_arch = "wasm32")]
async fn build_client_base<I, C>(
    infra: BuiltInfrastructure<I>,
    network_config: &C,
) -> Result<BaseNode<I, ClientNodeBehaviour<I>>>
where
    I: SwarmIdentity + Clone,
    C: SwarmNetworkConfig,
{
    let connection_limits = super::base::build_connection_limits(network_config);
    super::builder::build_base_node(infra, network_config, "Client node", move |pk, topology| {
        ClientNodeBehaviour::from_parts(pk, topology, connection_limits)
    })
    .await
}

/// Events from the client node behaviour.
#[allow(clippy::large_enum_variant)]
pub enum ClientNodeEvent {
    Identify(Box<identify::Event>),
    #[cfg(not(target_arch = "wasm32"))]
    AutonatClient(autonat::client::Event),
    #[cfg(not(target_arch = "wasm32"))]
    AutonatServer(autonat::server::Event),
    #[cfg(not(target_arch = "wasm32"))]
    Upnp(upnp::Event),
    #[cfg(not(target_arch = "wasm32"))]
    Mdns(mdns::Event),
    Topology(()),
    Client(ClientEvent),
}

impl From<Infallible> for ClientNodeEvent {
    fn from(event: Infallible) -> Self {
        // The connection-limits behaviour never emits events.
        match event {}
    }
}

impl From<identify::Event> for ClientNodeEvent {
    fn from(event: identify::Event) -> Self {
        ClientNodeEvent::Identify(Box::new(event))
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl From<autonat::client::Event> for ClientNodeEvent {
    fn from(event: autonat::client::Event) -> Self {
        ClientNodeEvent::AutonatClient(event)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl From<autonat::server::Event> for ClientNodeEvent {
    fn from(event: autonat::server::Event) -> Self {
        ClientNodeEvent::AutonatServer(event)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl From<upnp::Event> for ClientNodeEvent {
    fn from(event: upnp::Event) -> Self {
        ClientNodeEvent::Upnp(event)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl From<mdns::Event> for ClientNodeEvent {
    fn from(event: mdns::Event) -> Self {
        ClientNodeEvent::Mdns(event)
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
    client_event_tx: mpsc::Sender<ClientEvent>,
    client_command_rx: mpsc::Receiver<ClientCommand>,
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
                        .on_command(TopologyCommand::Dial(addr));
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

    /// Start listening and run the event loop with graceful shutdown support.
    pub async fn start_and_run(mut self, shutdown: GracefulShutdown) -> Result<()> {
        self.start_listening()?;
        self.run(shutdown).await
    }

    /// Run the event loop with graceful shutdown support.
    ///
    /// When the shutdown signal fires, the node will complete its current work
    /// and exit gracefully.
    pub async fn run(mut self, shutdown: GracefulShutdown) -> Result<()> {
        info!("Starting client node event loop");

        let mut topo_events = self.base.topology_handle.subscribe();
        let mut shutdown = std::pin::pin!(shutdown);

        loop {
            tokio::select! {
                guard = &mut shutdown => {
                    info!("Client node shutdown signal received");
                    self.base.swarm.behaviour_mut().topology.on_command(TopologyCommand::SavePeers);
                    drop(guard);
                    break;
                }
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

        info!("Client node shutdown complete");
        Ok(())
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
                self.handle_identify_event(*event);
            }
            #[cfg(not(target_arch = "wasm32"))]
            ClientNodeEvent::AutonatServer(event) => {
                super::base::handle_autonat_server_event(
                    &self.base.swarm.behaviour().topology,
                    event,
                );
            }
            #[cfg(not(target_arch = "wasm32"))]
            ClientNodeEvent::AutonatClient(event) => {
                super::base::handle_autonat_client_event(event);
            }
            #[cfg(not(target_arch = "wasm32"))]
            ClientNodeEvent::Upnp(event) => {
                super::base::handle_upnp_event(event);
            }
            #[cfg(not(target_arch = "wasm32"))]
            ClientNodeEvent::Mdns(event) => {
                let local_peer_id = *self.base.swarm.local_peer_id();
                super::base::handle_mdns_event(
                    local_peer_id,
                    &mut self.base.swarm.behaviour_mut().topology,
                    event,
                );
            }
            ClientNodeEvent::Topology(_) => {}
            ClientNodeEvent::Client(event) => {
                self.route_client_event(event);
            }
        }
    }

    fn handle_identify_event(&mut self, event: identify::Event) {
        super::base::handle_identify_event(&mut self.base.swarm.behaviour_mut().identify, event);
    }

    fn handle_topology_service_event(&mut self, event: TopologyEvent) {
        match event {
            TopologyEvent::PeerReady {
                overlay,
                peer_id,
                node_type,
                ..
            } => {
                self.base
                    .swarm
                    .behaviour_mut()
                    .client
                    .on_command(ClientCommand::ActivatePeer {
                        peer_id,
                        overlay,
                        node_type,
                    });
            }
            TopologyEvent::PeerDisconnected { .. } => {}
            TopologyEvent::PeerRejected { .. } => {}
            TopologyEvent::DepthChanged { .. } => {}
            TopologyEvent::PhaseChanged { .. } => {}
            TopologyEvent::DialFailed { .. } => {}
            TopologyEvent::PingCompleted { .. } => {}
        }
    }

    fn route_client_event(&self, event: ClientEvent) {
        if let Err(e) = self.client_event_tx.try_send(event) {
            warn!(%e, "Failed to send client event to service");
            metrics::counter!("swarm.client.events_dropped").increment(1);
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

/// Builder for ClientNode.
pub struct ClientNodeBuilder<I: SwarmIdentity + Clone> {
    identity: I,
    infra: Option<BuiltInfrastructure<I>>,
    kademlia_config: Option<KademliaConfig>,
    pseudosettle_event_tx: Option<mpsc::UnboundedSender<PseudosettleEvent>>,
    #[cfg(feature = "swap")]
    swap_event_tx: Option<mpsc::UnboundedSender<crate::protocol::SwapEvent>>,
}

impl<I: SwarmIdentity + Clone> ClientNodeBuilder<I> {
    pub fn new(identity: I) -> Self {
        Self {
            identity,
            infra: None,
            kademlia_config: None,
            pseudosettle_event_tx: None,
            #[cfg(feature = "swap")]
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

    /// Route swap wire events to the SWAP settlement service.
    ///
    /// When set, swap cheque events are forwarded to this channel so the
    /// settlement service can validate and credit received cheques and complete
    /// outbound settlements.
    #[cfg(feature = "swap")]
    pub fn with_swap_events(
        mut self,
        tx: mpsc::UnboundedSender<crate::protocol::SwapEvent>,
    ) -> Self {
        self.swap_event_tx = Some(tx);
        self
    }
}

impl<I: SwarmIdentity + Clone> ClientNodeBuilder<I> {
    pub async fn build<C>(
        self,
        network_config: &C,
        peer_store: Option<super::builder::PeerStore>,
    ) -> Result<(ClientNode<I>, ClientService, ClientHandle)>
    where
        I: vertex_swarm_spec::HasSpec,
        C: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig<Routing = KademliaConfig>,
    {
        info!("Initializing client P2P network...");

        let infra = match self.infra {
            Some(infra) => infra,
            None => {
                let topology_config =
                    TopologyConfig::new().with_kademlia(self.kademlia_config.unwrap_or_default());
                BuiltInfrastructure::from_config(
                    self.identity,
                    network_config,
                    topology_config,
                    peer_store,
                )?
            }
        };

        let mut base = build_client_base(infra, network_config).await?;

        // Register the local PeerId for address advertisement in handshakes
        base.swarm
            .behaviour()
            .topology
            .register_local_peer_id(*base.swarm.local_peer_id());

        if let Some(tx) = self.pseudosettle_event_tx {
            base.swarm
                .behaviour_mut()
                .client
                .route_pseudosettle_events(tx);
        }

        #[cfg(feature = "swap")]
        if let Some(tx) = self.swap_event_tx {
            base.swarm.behaviour_mut().client.route_swap_events(tx);
        }

        let executor = TaskExecutor::current();
        super::task::spawn_stats_task(
            Arc::new(base.topology_handle.clone()),
            Arc::clone(base.topology_handle.peer_manager().score_distribution()),
            super::stats::StatsConfig::default(),
            &executor,
        );

        let (command_tx, command_rx) =
            mpsc::channel(crate::client_service::DEFAULT_CHANNEL_CAPACITY);
        let (event_tx, event_rx) = mpsc::channel(crate::client_service::DEFAULT_CHANNEL_CAPACITY);

        let (client_service, client_handle) = ClientService::with_channels(command_tx, event_rx);

        let node = ClientNode {
            base,
            client_event_tx: event_tx,
            client_command_rx: command_rx,
        };

        Ok((node, client_service, client_handle))
    }
}
