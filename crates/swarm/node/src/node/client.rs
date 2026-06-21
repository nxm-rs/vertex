//! [`ClientNode`]: base topology protocols plus client protocols (pricing,
//! retrieval, pushsync, settlement) for reading from and writing to the network.

use std::convert::Infallible;
use std::sync::Arc;

use eyre::Result;
use futures::StreamExt;
use libp2p::connection_limits;
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

use vertex_swarm_api::SwarmLocalStore;

use super::base::BaseNode;
use super::builder::BuiltInfrastructure;
use super::nat::{NatBehaviour, NatEvent};
use crate::protocol::{
    BehaviourConfig as ClientBehaviourConfig, ClientBehaviour, ClientCommand, ClientEvent,
    PseudosettleEvent, StubForwarder,
};
use crate::{ClientHandle, ClientService};

/// Network behaviour for a client node (topology + client protocols).
#[derive(NetworkBehaviour)]
#[behaviour(to_swarm = "ClientNodeEvent")]
pub(crate) struct ClientNodeBehaviour<I: SwarmIdentity + Clone> {
    /// Connection caps (total, per-peer, pending). First so a denied connection
    /// is rejected before other behaviours allocate per-connection state.
    pub(crate) connection_limits: connection_limits::Behaviour,
    pub(crate) identify: identify::Behaviour,
    /// NAT traversal and LAN discovery as one platform sub-behaviour; a no-op in
    /// the browser, where a wasm client dials over websockets and never listens.
    pub(crate) nat: NatBehaviour,
    pub(crate) topology: TopologyBehaviour<I>,
    pub(crate) client: ClientBehaviour,
}

impl<I: SwarmIdentity + Clone> ClientNodeBehaviour<I> {
    pub(crate) fn from_parts(
        local_public_key: PublicKey,
        topology: TopologyBehaviour<I>,
        nat: NatBehaviour,
        connection_limits: connection_limits::Behaviour,
        store: Arc<dyn SwarmLocalStore>,
    ) -> Self {
        let agent_versions = topology.agent_versions();
        Self {
            connection_limits,
            // Identify advertises addresses scoped per peer (see
            // `addresses_for_remote`), so a public peer never receives our
            // private or loopback addresses.
            identify: identify::Behaviour::new(
                identify::Config::new(local_public_key),
                agent_versions,
            ),
            nat,
            topology,
            // Cache-only client never relays: the stub forwarder resets the
            // substream on cache miss and every inbound pushsync. The real relay
            // is installed by `enable_forwarding`.
            client: ClientBehaviour::new(
                ClientBehaviourConfig::default(),
                store,
                Arc::new(StubForwarder),
            ),
        }
    }
}

/// Assemble the client base node, wiring the platform NAT sub-behaviour into
/// the composite.
async fn build_client_base<I, C>(
    infra: BuiltInfrastructure<I>,
    network_config: &C,
    store: Arc<dyn SwarmLocalStore>,
) -> Result<BaseNode<I, ClientNodeBehaviour<I>>>
where
    I: SwarmIdentity + Clone,
    C: SwarmNetworkConfig,
{
    let connection_limits = super::base::build_connection_limits(network_config);
    super::builder::build_base_node(infra, network_config, "Client node", move |pk, topology| {
        let nat = NatBehaviour::from_config(network_config, pk.to_peer_id());
        ClientNodeBehaviour::from_parts(pk, topology, nat, connection_limits, store)
    })
    .await
}

/// Events from the client node behaviour.
#[allow(clippy::large_enum_variant)]
pub enum ClientNodeEvent {
    Identify(Box<identify::Event>),
    Nat(NatEvent),
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

impl From<NatEvent> for ClientNodeEvent {
    fn from(event: NatEvent) -> Self {
        ClientNodeEvent::Nat(event)
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

/// A Swarm client node with pricing, retrieval, and pushsync protocols. Unlike
/// [`BootNode`](super::BootNode), it can read from and write to the network.
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

    /// Enable multi-hop forwarding (relay), replacing the default stub so a
    /// retrieval cache miss forwards to a strictly-closer peer and an inbound
    /// pushsync relays toward the chunk's neighbourhood, accounting both legs.
    ///
    /// Must be called during node assembly, before the event loop accepts
    /// connections: a handler created earlier captures the stub.
    pub fn enable_forwarding<T, A>(
        &mut self,
        topology: Arc<T>,
        accounting: Arc<A>,
        handle: ClientHandle,
    ) where
        T: vertex_swarm_api::SwarmTopologyRouting
            + vertex_swarm_api::SwarmTopologyState
            + vertex_swarm_api::SwarmTopologyReporting
            + Send
            + Sync
            + 'static,
        A: vertex_swarm_api::SwarmClientAccounting + Send + Sync + 'static,
    {
        use vertex_swarm_api::SwarmSpec;

        let local = self.overlay_address();
        let network_id = topology.identity().spec().network_id();
        let reporter = topology.reporter();
        // Network id recovers an inbound receipt's signer; set it before any
        // handler is created (handlers clone the config at connection setup).
        self.base
            .swarm
            .behaviour_mut()
            .client
            .set_network_id(network_id);
        let forwarder = Arc::new(crate::protocol::NetworkForwarder::new(
            local, topology, accounting, handle, reporter,
        ));
        self.base
            .swarm
            .behaviour_mut()
            .client
            .set_forwarder(forwarder);
    }

    /// Install the storer ingest capability, turning the inbound pushsync path
    /// from forward-only into store-and-sign for chunks this node is responsible
    /// for: a responsible delivery is put into `reserve` and acknowledged with a
    /// receipt signed by the identity key, bound to its nonce. Non-responsible
    /// deliveries still forward (see
    /// [`enable_forwarding`](Self::enable_forwarding)).
    ///
    /// Must be called during node assembly, before the event loop accepts
    /// connections: a handler created earlier does not capture the capability.
    pub fn enable_storage(&mut self, reserve: Arc<dyn vertex_swarm_api::ReserveStore>) {
        let capability = crate::protocol::StorerCapability::new(reserve, self.base.identity());
        self.base
            .swarm
            .behaviour_mut()
            .client
            .set_storer(capability);
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
                    match result {
                        Ok(event) => self.handle_topology_service_event(event),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                            warn!(skipped, "Client node lagged behind topology events");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            info!("Topology event channel closed, shutting down client node");
                            break;
                        }
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
            ClientNodeEvent::Nat(event) => {
                let local_peer_id = *self.base.swarm.local_peer_id();
                super::nat::handle_nat_event(
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
    store: Option<Arc<dyn SwarmLocalStore>>,
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
            store: None,
            pseudosettle_event_tx: None,
            #[cfg(feature = "swap")]
            swap_event_tx: None,
        }
    }

    pub fn with_infrastructure(mut self, infra: BuiltInfrastructure<I>) -> Self {
        self.infra = Some(infra);
        self
    }

    /// Inject the client chunk cache (served for inbound retrievals and used for
    /// the client's own deliveries). Defaults to an in-memory cache when unset.
    pub fn with_store(mut self, store: Arc<dyn SwarmLocalStore>) -> Self {
        self.store = Some(store);
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

    /// Route swap cheque events to the SWAP settlement service for validation,
    /// crediting, and outbound settlement.
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

        let store: Arc<dyn SwarmLocalStore> = self.store.unwrap_or_else(|| {
            Arc::new(vertex_swarm_localstore::ChunkStore::with_budget(
                vertex_swarm_localstore::DEFAULT_CACHE_BUDGET_BYTES as usize,
                vertex_swarm_localstore::DEFAULT_SOC_CACHE_TTL_NS,
            ))
        });

        let mut base = build_client_base(infra, network_config, Arc::clone(&store)).await?;

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
        let client_service = client_service.with_store(store);

        let node = ClientNode {
            base,
            client_event_tx: event_tx,
            client_command_rx: command_rx,
        };

        Ok((node, client_service, client_handle))
    }
}
