//! StorerNode: a client node plus the pullsync protocol.
//!
//! The behaviour composite is [`ClientNodeBehaviour`](super::client)'s infra
//! sub-behaviours plus a [`StorerBehaviour`] (client + pullsync) in place of the
//! bare client behaviour. The inbound pullsync syncer serves cursors and ranges
//! from the reserve; the outbound puller fills the reserve from neighbours and is
//! driven over a command channel the run loop dispatches to the pullsync
//! sub-behaviour, with delivered [`PullsyncEvent`]s forwarded back to it.

use std::convert::Infallible;
use std::sync::Arc;

use eyre::Result;
use futures::StreamExt;
use libp2p::connection_limits;
use libp2p::{Multiaddr, PeerId, identity::PublicKey, swarm::NetworkBehaviour, swarm::SwarmEvent};
use nectar_primitives::SwarmAddress;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use vertex_swarm_api::{
    PullStorage, SwarmIdentity, SwarmLocalStore, SwarmNetworkConfig, SwarmPeerConfig,
    SwarmRoutingConfig,
};
use vertex_swarm_net_identify as identify;
use vertex_swarm_primitives::Bin;
use vertex_swarm_puller::{PullerHandle, PullsyncControl};
use vertex_swarm_storer_behaviour::{
    PullsyncBehaviour, PullsyncEvent, StorerBehaviour, StorerBehaviourEvent,
};
use vertex_swarm_topology::{
    KademliaConfig, TopologyBehaviour, TopologyCommand, TopologyConfig, TopologyEvent,
    TopologyHandle,
};
use vertex_tasks::GracefulShutdown;
use vertex_tasks::TaskExecutor;

use super::base::BaseNode;
use super::builder::BuiltInfrastructure;
use super::nat::{NatBehaviour, NatEvent};
use crate::protocol::{
    BehaviourConfig as ClientBehaviourConfig, ClientBehaviour, ClientCommand, ClientEvent,
    PseudosettleEvent, StubForwarder,
};
use crate::{ClientHandle, ClientService};

/// Outbound pullsync command the run loop dispatches to the pullsync
/// sub-behaviour. Mirrors the `ClientCommand` path: the puller's
/// [`PullsyncControl`] enqueues these and the swarm loop drains them.
enum PullsyncCommand {
    FetchCursors {
        peer: PeerId,
        request_id: u64,
    },
    SyncRange {
        peer: PeerId,
        request_id: u64,
        bin: Bin,
        start: u64,
    },
}

/// [`PullsyncControl`] bridge: the puller's outbound command surface, sending
/// into the run loop's command channel. Mirrors [`ClientHandle`]'s command path.
#[derive(Clone)]
pub struct StorerPullsyncControl {
    command_tx: mpsc::Sender<PullsyncCommand>,
}

impl PullsyncControl for StorerPullsyncControl {
    fn fetch_cursors(&self, peer: PeerId, request_id: u64) {
        if self
            .command_tx
            .try_send(PullsyncCommand::FetchCursors { peer, request_id })
            .is_err()
        {
            metrics::counter!("swarm.pullsync.commands_dropped").increment(1);
        }
    }

    fn sync_range(&self, peer: PeerId, request_id: u64, bin: Bin, start: u64) {
        if self
            .command_tx
            .try_send(PullsyncCommand::SyncRange {
                peer,
                request_id,
                bin,
                start,
            })
            .is_err()
        {
            metrics::counter!("swarm.pullsync.commands_dropped").increment(1);
        }
    }
}

/// Network behaviour for a storer node: the client node's infra sub-behaviours
/// plus the storer protocol tier (client + pullsync).
#[derive(NetworkBehaviour)]
#[behaviour(to_swarm = "StorerNodeEvent")]
pub(crate) struct StorerNodeBehaviour<I: SwarmIdentity + Clone> {
    /// Connection caps (total, per-peer, pending). First so a denied connection
    /// is rejected before other behaviours allocate per-connection state.
    pub(crate) connection_limits: connection_limits::Behaviour,
    pub(crate) identify: identify::Behaviour,
    /// NAT traversal and LAN discovery, native only.
    pub(crate) nat: NatBehaviour,
    pub(crate) topology: TopologyBehaviour<I>,
    /// Client protocols plus pullsync, served from the reserve.
    pub(crate) storer: StorerBehaviour,
}

impl<I: SwarmIdentity + Clone> StorerNodeBehaviour<I> {
    fn from_parts(
        local_public_key: PublicKey,
        topology: TopologyBehaviour<I>,
        nat: NatBehaviour,
        connection_limits: connection_limits::Behaviour,
        store: Arc<dyn SwarmLocalStore>,
        pullsync_storage: Arc<dyn PullStorage>,
    ) -> Self {
        let agent_versions = topology.agent_versions();
        let client = ClientBehaviour::new(
            ClientBehaviourConfig::default(),
            store,
            Arc::new(StubForwarder),
        );
        Self {
            connection_limits,
            identify: identify::Behaviour::new(
                identify::Config::new(local_public_key),
                agent_versions,
            ),
            nat,
            topology,
            storer: StorerBehaviour {
                client,
                pullsync: PullsyncBehaviour::new(pullsync_storage),
            },
        }
    }
}

/// Assemble the storer base node, wiring the platform NAT sub-behaviour and the
/// reserve-backed pullsync syncer into the composite.
async fn build_storer_base<I, C>(
    infra: BuiltInfrastructure<I>,
    network_config: &C,
    store: Arc<dyn SwarmLocalStore>,
    pullsync_storage: Arc<dyn PullStorage>,
) -> Result<BaseNode<I, StorerNodeBehaviour<I>>>
where
    I: SwarmIdentity + Clone,
    C: SwarmNetworkConfig,
{
    let connection_limits = super::base::build_connection_limits(network_config);
    super::builder::build_base_node(infra, network_config, "Storer node", move |pk, topology| {
        let nat = NatBehaviour::from_config(network_config, pk.to_peer_id());
        StorerNodeBehaviour::from_parts(
            pk,
            topology,
            nat,
            connection_limits,
            store,
            pullsync_storage,
        )
    })
    .await
}

/// Events from the storer node behaviour.
#[allow(clippy::large_enum_variant)]
pub enum StorerNodeEvent {
    Identify(Box<identify::Event>),
    Nat(NatEvent),
    Topology(()),
    Client(ClientEvent),
    Pullsync(PullsyncEvent),
}

impl From<Infallible> for StorerNodeEvent {
    fn from(event: Infallible) -> Self {
        // The connection-limits behaviour never emits events.
        match event {}
    }
}

impl From<identify::Event> for StorerNodeEvent {
    fn from(event: identify::Event) -> Self {
        StorerNodeEvent::Identify(Box::new(event))
    }
}

impl From<NatEvent> for StorerNodeEvent {
    fn from(event: NatEvent) -> Self {
        StorerNodeEvent::Nat(event)
    }
}

impl From<()> for StorerNodeEvent {
    fn from(_: ()) -> Self {
        StorerNodeEvent::Topology(())
    }
}

impl From<StorerBehaviourEvent> for StorerNodeEvent {
    fn from(event: StorerBehaviourEvent) -> Self {
        match event {
            StorerBehaviourEvent::Client(event) => StorerNodeEvent::Client(event),
            StorerBehaviourEvent::Pullsync(event) => StorerNodeEvent::Pullsync(event),
        }
    }
}

/// A full Swarm storer node: client protocols plus reserve storage and the
/// neighbourhood pullsync (inbound syncer and outbound puller).
pub struct StorerNode<I: SwarmIdentity + Clone> {
    base: BaseNode<I, StorerNodeBehaviour<I>>,
    client_event_tx: mpsc::Sender<ClientEvent>,
    client_command_rx: mpsc::Receiver<ClientCommand>,
    /// Outbound pullsync commands from the puller, dispatched to the pullsync
    /// sub-behaviour in the run loop.
    pullsync_command_rx: mpsc::Receiver<PullsyncCommand>,
    /// Delivered pullsync events forwarded to the running puller; `None` until
    /// [`set_puller`](Self::set_puller) wires it.
    puller: Option<PullerHandle>,
}

impl<I: SwarmIdentity + Clone> StorerNode<I> {
    pub fn builder(identity: I) -> StorerNodeBuilder<I> {
        StorerNodeBuilder::new(identity)
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

    /// Forward delivered [`PullsyncEvent`]s to this running puller. Must be set
    /// before the event loop runs, or range deliveries are dropped.
    pub fn set_puller(&mut self, puller: PullerHandle) {
        self.puller = Some(puller);
    }

    /// Enable multi-hop forwarding (relay) on the client sub-behaviour. See
    /// [`ClientNode::enable_forwarding`](super::ClientNode::enable_forwarding).
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
        self.base
            .swarm
            .behaviour_mut()
            .storer
            .client
            .set_network_id(network_id);
        let forwarder = Arc::new(crate::protocol::NetworkForwarder::new(
            local, topology, accounting, handle, reporter,
        ));
        self.base
            .swarm
            .behaviour_mut()
            .storer
            .client
            .set_forwarder(forwarder);
    }

    /// Install the storer ingest capability on the client sub-behaviour. See
    /// [`ClientNode::enable_storage`](super::ClientNode::enable_storage).
    pub fn enable_storage(&mut self, reserve: Arc<dyn vertex_swarm_api::ReserveStore>) {
        let signer: Arc<dyn vertex_swarm_primitives::OverlaySigner + Send + Sync> =
            Arc::new(self.base.identity().clone());
        let capability = crate::protocol::StorerCapability::new(reserve, signer);
        self.base
            .swarm
            .behaviour_mut()
            .storer
            .client
            .set_storer(capability);
    }

    pub fn topology_command(&mut self, command: TopologyCommand) {
        self.base.swarm.behaviour_mut().topology.on_command(command);
    }

    /// Dial peers from multiaddr strings. Returns the number of dials initiated.
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

    pub async fn start_and_run(mut self, shutdown: GracefulShutdown) -> Result<()> {
        self.start_listening()?;
        self.run(shutdown).await
    }

    /// Run the event loop with graceful shutdown support.
    pub async fn run(mut self, shutdown: GracefulShutdown) -> Result<()> {
        info!("Starting storer node event loop");

        let mut topo_events = self.base.topology_handle.subscribe();
        let mut shutdown = std::pin::pin!(shutdown);

        loop {
            tokio::select! {
                guard = &mut shutdown => {
                    info!("Storer node shutdown signal received");
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

                Some(command) = self.pullsync_command_rx.recv() => {
                    self.handle_pullsync_command(command);
                }

                result = topo_events.recv() => {
                    match result {
                        Ok(event) => self.handle_topology_service_event(event),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                            warn!(skipped, "Storer node lagged behind topology events");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            info!("Topology event channel closed, shutting down storer node");
                            break;
                        }
                    }
                }
            }
        }

        info!("Storer node shutdown complete");
        Ok(())
    }

    fn handle_swarm_event(&mut self, event: SwarmEvent<StorerNodeEvent>) {
        if let Some(SwarmEvent::Behaviour(behaviour_event)) =
            self.base.handle_swarm_event_common(event)
        {
            self.handle_behaviour_event(behaviour_event);
        }
    }

    fn handle_behaviour_event(&mut self, event: StorerNodeEvent) {
        match event {
            StorerNodeEvent::Identify(event) => {
                self.handle_identify_event(*event);
            }
            StorerNodeEvent::Nat(event) => {
                let local_peer_id = *self.base.swarm.local_peer_id();
                super::nat::handle_nat_event(
                    local_peer_id,
                    &mut self.base.swarm.behaviour_mut().topology,
                    event,
                );
            }
            StorerNodeEvent::Topology(_) => {}
            StorerNodeEvent::Client(event) => {
                self.route_client_event(event);
            }
            StorerNodeEvent::Pullsync(event) => {
                self.route_pullsync_event(event);
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
                trusted,
                ..
            } => {
                self.base.swarm.behaviour_mut().storer.client.on_command(
                    ClientCommand::ActivatePeer {
                        peer_id,
                        overlay,
                        node_type,
                        trusted,
                    },
                );
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

    fn route_pullsync_event(&self, event: PullsyncEvent) {
        let Some(puller) = &self.puller else {
            return;
        };
        if puller.deliver(event).is_err() {
            metrics::counter!("swarm.pullsync.events_dropped").increment(1);
        }
    }

    fn handle_client_command(&mut self, command: ClientCommand) {
        self.base
            .swarm
            .behaviour_mut()
            .storer
            .client
            .on_command(command);
    }

    fn handle_pullsync_command(&mut self, command: PullsyncCommand) {
        let (peer, request_id) = match &command {
            PullsyncCommand::FetchCursors { peer, request_id }
            | PullsyncCommand::SyncRange {
                peer, request_id, ..
            } => (*peer, *request_id),
        };

        // A `NotifyHandler` for an unconnected peer is dropped silently, leaving
        // the puller to wait out its full response timeout. Synthesize the
        // failure so it abandons the target at once.
        if !self.base.swarm.is_connected(&peer) {
            self.route_pullsync_event(PullsyncEvent::Failed {
                peer,
                request_id,
                failure: vertex_swarm_storer_behaviour::PullsyncFailure::Stream(
                    "peer not connected".into(),
                ),
            });
            return;
        }

        let pullsync = &mut self.base.swarm.behaviour_mut().storer.pullsync;
        match command {
            PullsyncCommand::FetchCursors { peer, request_id } => {
                pullsync.fetch_cursors(peer, request_id)
            }
            PullsyncCommand::SyncRange {
                peer,
                request_id,
                bin,
                start,
            } => pullsync.sync_range(peer, request_id, bin, start),
        }
    }

    pub fn connected_peers(&self) -> usize {
        self.base.connected_peers()
    }

    pub fn is_connected(&self) -> bool {
        self.base.is_connected()
    }
}

/// Default channel capacity for the pullsync command bridge.
const PULLSYNC_COMMAND_CAPACITY: usize = 256;

/// Builder for StorerNode.
pub struct StorerNodeBuilder<I: SwarmIdentity + Clone> {
    identity: I,
    kademlia_config: Option<KademliaConfig>,
    store: Option<Arc<dyn SwarmLocalStore>>,
    pullsync_storage: Option<Arc<dyn PullStorage>>,
    pseudosettle_event_tx: Option<mpsc::UnboundedSender<PseudosettleEvent>>,
    #[cfg(feature = "swap")]
    swap_event_tx: Option<mpsc::UnboundedSender<crate::protocol::SwapEvent>>,
}

impl<I: SwarmIdentity + Clone> StorerNodeBuilder<I> {
    pub fn new(identity: I) -> Self {
        Self {
            identity,
            kademlia_config: None,
            store: None,
            pullsync_storage: None,
            pseudosettle_event_tx: None,
            #[cfg(feature = "swap")]
            swap_event_tx: None,
        }
    }

    pub fn with_kademlia_config(mut self, kademlia_config: KademliaConfig) -> Self {
        self.kademlia_config = Some(kademlia_config);
        self
    }

    /// Inject the retrieval-serve view (the cache-then-reserve store).
    pub fn with_store(mut self, store: Arc<dyn SwarmLocalStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// Inject the reserve snapshot the inbound pullsync syncer serves from.
    pub fn with_pullsync_storage(mut self, storage: Arc<dyn PullStorage>) -> Self {
        self.pullsync_storage = Some(storage);
        self
    }

    pub fn with_pseudosettle_events(
        mut self,
        tx: mpsc::UnboundedSender<PseudosettleEvent>,
    ) -> Self {
        self.pseudosettle_event_tx = Some(tx);
        self
    }

    #[cfg(feature = "swap")]
    pub fn with_swap_events(
        mut self,
        tx: mpsc::UnboundedSender<crate::protocol::SwapEvent>,
    ) -> Self {
        self.swap_event_tx = Some(tx);
        self
    }
}

impl<I: SwarmIdentity + Clone> StorerNodeBuilder<I> {
    /// Build the StorerNode, ClientService, command handle, and the pullsync
    /// control the puller drives.
    pub async fn build<C>(
        self,
        network_config: &C,
        peer_store: Option<super::builder::PeerStore>,
    ) -> Result<(
        StorerNode<I>,
        ClientService,
        ClientHandle,
        StorerPullsyncControl,
    )>
    where
        I: vertex_swarm_spec::HasSpec,
        C: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig<Routing = KademliaConfig>,
    {
        info!("Initializing storer P2P network...");

        let pullsync_storage = self
            .pullsync_storage
            .ok_or_else(|| eyre::eyre!("storer node requires a pullsync reserve snapshot"))?;

        let topology_config =
            TopologyConfig::new().with_kademlia(self.kademlia_config.unwrap_or_default());
        let infra = BuiltInfrastructure::from_config(
            self.identity,
            network_config,
            topology_config,
            peer_store,
        )?;

        let store: Arc<dyn SwarmLocalStore> = self.store.unwrap_or_else(|| {
            Arc::new(vertex_swarm_localstore::ChunkStore::with_budget(
                vertex_swarm_localstore::DEFAULT_CACHE_BUDGET_BYTES as usize,
                vertex_swarm_localstore::DEFAULT_SOC_CACHE_TTL_NS,
            ))
        });

        let mut base =
            build_storer_base(infra, network_config, Arc::clone(&store), pullsync_storage).await?;

        base.swarm
            .behaviour()
            .topology
            .register_local_peer_id(*base.swarm.local_peer_id());

        if let Some(tx) = self.pseudosettle_event_tx {
            base.swarm
                .behaviour_mut()
                .storer
                .client
                .route_pseudosettle_events(tx);
        }

        #[cfg(feature = "swap")]
        if let Some(tx) = self.swap_event_tx {
            base.swarm
                .behaviour_mut()
                .storer
                .client
                .route_swap_events(tx);
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
        let (pullsync_command_tx, pullsync_command_rx) = mpsc::channel(PULLSYNC_COMMAND_CAPACITY);

        let (client_service, client_handle) = ClientService::with_channels(command_tx, event_rx);
        let client_service = client_service.with_store(store);
        let pullsync_control = StorerPullsyncControl {
            command_tx: pullsync_command_tx,
        };

        let node = StorerNode {
            base,
            client_event_tx: event_tx,
            client_command_rx: command_rx,
            pullsync_command_rx,
            puller: None,
        };

        Ok((node, client_service, client_handle, pullsync_control))
    }
}
