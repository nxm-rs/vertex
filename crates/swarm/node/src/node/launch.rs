//! Fluent launcher for an embedded Swarm client node.
//!
//! [`ClientLauncher`] is the lightweight entry point shared by native
//! embedders and the browser client: no database, no chain provider, no RPC
//! server. It composes a [`ClientNode`] from an identity and a handful of
//! network knobs, wires the shared client core (pseudosettle accounting,
//! candidate selector, outbound throttle, relay forwarder), spawns the node
//! run loop, the client service, the pseudosettle settlement service, and the
//! peer-manager tick on the current [`TaskExecutor`], and hands back a
//! [`LaunchedClient`] with the handles a caller needs to observe the topology
//! and issue chunk reads and writes. Settlement here is pseudosettle only; the
//! full native stack (storage, chain, swap, RPC) goes through
//! `vertex-swarm-builder` instead.

use std::sync::Arc;
use std::time::Duration;

use eyre::{Result, WrapErr};
use libp2p::{Multiaddr, PeerId};
use nectar_primitives::SwarmAddress;
use vertex_swarm_accounting::DefaultBandwidthConfig;
use vertex_swarm_api::{
    DefaultPeerConfig, PeerReporter, SwarmClientAccounting, SwarmLocalStore, SwarmNetworkConfig,
    SwarmPeerConfig, SwarmRoutingConfig,
};
use vertex_swarm_identity::Identity;
use vertex_swarm_localstore::{
    ChunkStore, DEFAULT_CACHE_BUDGET_BYTES, DEFAULT_SOC_CACHE_TTL_NS, SystemClock,
};
use vertex_swarm_peer_manager::{DEFAULT_TICK_INTERVAL, spawn_peer_manager_task};
use vertex_swarm_spec::HasSpec;
use vertex_swarm_topology::{KademliaConfig, TopologyHandle};
use vertex_tasks::TaskExecutor;

use super::client::ClientNode;
use super::core::{ClientCoreCtx, PseudosettleWiring, SharedAccounting, assemble_client_core};
use crate::ClientHandle;

/// Default connection idle timeout for a launched client.
const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Default transport-layer cap on established connections.
///
/// A saturated routing table sits comfortably below this; it is a resource
/// backstop, not a topology knob.
const DEFAULT_MAX_PEERS: usize = 400;

/// Network configuration assembled from the launcher's fields.
///
/// A launched client is dial-only: it carries no listen addresses and leaves
/// mDNS, UPnP, and AutoNAT off. This is the trimmed counterpart to the
/// CLI-driven `NetworkConfig` the full native builder uses.
struct LaunchNetworkConfig {
    bootnodes: Vec<Multiaddr>,
    peer: DefaultPeerConfig,
    routing: KademliaConfig,
    max_peers: usize,
    idle_timeout: Duration,
}

impl SwarmNetworkConfig for LaunchNetworkConfig {
    fn listen_addrs(&self) -> &[Multiaddr] {
        &[]
    }

    fn bootnodes(&self) -> &[Multiaddr] {
        &self.bootnodes
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

    fn nat_auto_enabled(&self) -> bool {
        false
    }

    fn autonat_enabled(&self) -> bool {
        false
    }

    fn upnp_enabled(&self) -> bool {
        false
    }

    fn mdns_enabled(&self) -> bool {
        false
    }
}

impl SwarmPeerConfig for LaunchNetworkConfig {
    type Peers = DefaultPeerConfig;

    fn peers(&self) -> &Self::Peers {
        &self.peer
    }
}

impl SwarmRoutingConfig for LaunchNetworkConfig {
    type Routing = KademliaConfig;

    fn routing(&self) -> &Self::Routing {
        &self.routing
    }
}

/// Fluent launcher for an embedded Swarm client node.
///
/// This is the lightweight entry point: no database, no chain provider, no RPC
/// server. The full native stack goes through `vertex-swarm-builder`.
///
/// The launched node is dial-only on both targets: it opens no listeners and
/// runs no NAT traversal or LAN discovery. On native that suits embedders that
/// only read from and write to the network; in the browser it is the only
/// possible shape.
///
/// # Example
///
/// ```ignore
/// use vertex_swarm_node::ClientLauncher;
///
/// let launched = ClientLauncher::new(identity)
///     .with_bootnodes(bootnodes)
///     .launch()
///     .await?;
/// let topology = launched.topology().clone();
/// ```
pub struct ClientLauncher {
    identity: Arc<Identity>,
    bootnodes: Vec<Multiaddr>,
    kademlia: KademliaConfig,
    bandwidth: DefaultBandwidthConfig,
    max_peers: usize,
    idle_timeout: Duration,
}

impl ClientLauncher {
    /// Create a launcher for the given identity with default settings.
    #[must_use]
    pub fn new(identity: impl Into<Arc<Identity>>) -> Self {
        Self {
            identity: identity.into(),
            bootnodes: Vec::new(),
            kademlia: KademliaConfig::default(),
            bandwidth: DefaultBandwidthConfig::default(),
            max_peers: DEFAULT_MAX_PEERS,
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
        }
    }

    /// Set the bootnode multiaddrs to dial at startup.
    ///
    /// When left empty, the launcher falls back to the bootnodes baked into
    /// the identity's network spec. Topology resolves those per platform: the
    /// system resolver natively, DNS-over-HTTPS in the browser.
    #[must_use]
    pub fn with_bootnodes(mut self, bootnodes: impl IntoIterator<Item = Multiaddr>) -> Self {
        self.bootnodes = bootnodes.into_iter().collect();
        self
    }

    /// Set the Kademlia routing configuration.
    #[must_use]
    pub fn with_kademlia(mut self, config: KademliaConfig) -> Self {
        self.kademlia = config;
        self
    }

    /// Set the bandwidth accounting configuration.
    ///
    /// Drives the pseudosettle allowance, the per-chunk price, and the outbound
    /// self-throttle sizing. Defaults to [`DefaultBandwidthConfig::default`].
    #[must_use]
    pub fn with_bandwidth(mut self, bandwidth: DefaultBandwidthConfig) -> Self {
        self.bandwidth = bandwidth;
        self
    }

    /// Set the transport-layer cap on established connections.
    #[must_use]
    pub fn with_max_peers(mut self, max: usize) -> Self {
        self.max_peers = max;
        self
    }

    /// Set the connection idle timeout.
    #[must_use]
    pub fn with_idle_timeout(mut self, timeout: Duration) -> Self {
        self.idle_timeout = timeout;
        self
    }

    /// Build and start the client node, returning its handles.
    ///
    /// Assembles a [`ClientNode`] over the platform transport (TCP with DNS
    /// natively, secure websockets in the browser), wires the shared client
    /// core (pseudosettle accounting, candidate selector, outbound throttle,
    /// and the relay forwarder), spawns the node run loop, the client service,
    /// the pseudosettle settlement service, and the peer-manager tick on the
    /// current [`TaskExecutor`], and returns a [`LaunchedClient`]. The node
    /// dials its bootnodes as part of startup; from there the Kademlia routing
    /// table fills on its own.
    ///
    /// Settlement is pseudosettle only; swap stays a native-builder concern.
    ///
    /// The returned handles own nothing the spawned tasks need, so the client
    /// keeps running after they are dropped. Shutdown goes through the task
    /// executor's graceful-shutdown signal.
    ///
    /// # Errors
    ///
    /// Returns an error if the swarm fails to assemble (transport or behaviour
    /// construction). Failures after spawn, including the run loop exiting
    /// with an error, are logged by the spawned task.
    pub async fn launch(self) -> Result<LaunchedClient> {
        let config = LaunchNetworkConfig {
            bootnodes: self.bootnodes,
            peer: DefaultPeerConfig::default(),
            routing: self.kademlia.clone(),
            max_peers: self.max_peers,
            idle_timeout: self.idle_timeout,
        };

        let spec = Arc::clone(HasSpec::spec(&self.identity));

        // The launcher owns the in-memory client cache so callers can read it
        // back; the node serves inbound retrievals and the client's own
        // deliveries from the same store.
        let store = Arc::new(ChunkStore::with_budget(
            DEFAULT_CACHE_BUDGET_BYTES as usize,
            DEFAULT_SOC_CACHE_TTL_NS,
        ));

        // Pseudosettle wiring is prepared before the node so the event sink
        // routes wire events at build time and the provider embeds in the
        // accounting the core assembles.
        let (pseudosettle_provider, pseudosettle_wiring) =
            PseudosettleWiring::prepare(&self.bandwidth);

        let store_dyn: Arc<dyn SwarmLocalStore> = store.clone();
        let (mut node, client_service, client_handle) =
            ClientNode::builder(Arc::clone(&self.identity))
                .with_kademlia_config(self.kademlia)
                .with_store(store_dyn)
                .with_pseudosettle_events(pseudosettle_wiring.event_sender())
                .build(&config, None)
                .await
                .wrap_err("failed to build client node")?;

        let topology = node.topology_handle().clone();
        let overlay = node.overlay_address();
        let peer_id = *node.local_peer_id();

        // The peer manager is the reporting authority: accounting and the
        // pseudosettle service report violations through it.
        let reporter: Arc<dyn PeerReporter> = topology.peer_manager().clone();

        // Assemble the shared client middle (accounting, selector, throttle,
        // service) the native builder also goes through; the embedded client
        // carries no native-only settlement, so the swap providers are absent.
        let core = assemble_client_core(ClientCoreCtx {
            spec,
            identity: Arc::clone(&self.identity),
            bandwidth: self.bandwidth,
            topology: topology.clone(),
            client_service,
            client_handle: client_handle.clone(),
            pseudosettle_provider,
            extra_settlement: Vec::new(),
            reporter: Arc::clone(&reporter),
        });

        // Multi-hop forwarding must precede the event loop: a handler created
        // earlier captures the stub forwarder. The node is borrowed mutably
        // here and then moved into the run loop below, so the borrow and the
        // move stay in this scope.
        node.enable_forwarding(
            Arc::new(topology.clone()),
            Arc::clone(&core.accounting),
            core.throttled_handle.clone(),
        );

        let executor = TaskExecutor::current();

        // The peer manager tick is pure data and `Send`, so it goes through
        // the ordinary spawner.
        spawn_peer_manager_task(
            topology.peer_manager().clone(),
            DEFAULT_TICK_INTERVAL,
            &executor,
        );

        // The client service drives the retrieval and pushsync request paths.
        // It must run even for callers that never issue downloads, so the node
        // can answer the protocols its peers speak during topology building.
        // Its task is `Send`, so it goes through the ordinary service spawner.
        executor.spawn_service("swarm.client_service", core.client_service);

        // The pseudosettle settlement service applies time-based refresh over
        // the same shared accounting and forwards our outbound settlement to
        // the node through the unthrottled handle.
        pseudosettle_wiring.spawn(
            &executor,
            core.accounting.bandwidth().clone(),
            core.client_handle.clone(),
            reporter,
        );

        // The node run loop owns the libp2p swarm. It starts listening (a
        // no-op for a dial-only client) and then dials bootnodes and services
        // the event loop for the session.
        spawn_node_run_loop(&executor, node);

        Ok(LaunchedClient {
            topology,
            client: core.throttled_handle,
            accounting: core.accounting,
            store,
            overlay,
            peer_id,
        })
    }
}

/// Spawn the node run loop on the executor.
///
/// The native swarm and its futures are `Send`, so the run loop spawns as a
/// critical task on the tokio runtime and participates in graceful shutdown.
#[cfg(not(target_arch = "wasm32"))]
fn spawn_node_run_loop(executor: &TaskExecutor, node: ClientNode<Arc<Identity>>) {
    executor.spawn_critical_with_graceful_shutdown_signal(
        "swarm.client_node",
        move |shutdown| async move {
            if let Err(e) = node.start_and_run(shutdown).await {
                tracing::error!(error = %e, "client node exited with error");
            }
        },
    );
}

/// Spawn the node run loop on the browser event loop.
///
/// The browser swarm's websocket transport futures are `!Send`, so the run
/// loop goes through the executor's local spawner instead of the Send-bounded
/// critical one.
#[cfg(target_arch = "wasm32")]
fn spawn_node_run_loop(executor: &TaskExecutor, node: ClientNode<Arc<Identity>>) {
    executor.spawn_local_with_graceful_shutdown_signal(
        "swarm.client_node",
        move |shutdown| async move {
            if let Err(e) = node.start_and_run(shutdown).await {
                tracing::error!(error = %e, "client node exited with error");
            }
        },
    );
}

/// Handles to a running embedded client node.
///
/// Returned by [`ClientLauncher::launch`]. The spawned tasks do not depend on
/// this value staying alive; dropping it leaves the node running until the
/// executor shuts down.
pub struct LaunchedClient {
    topology: TopologyHandle<Arc<Identity>>,
    client: ClientHandle,
    accounting: SharedAccounting,
    store: Arc<ChunkStore<SystemClock>>,
    overlay: SwarmAddress,
    peer_id: PeerId,
}

impl LaunchedClient {
    /// Topology handle for readiness polling and
    /// [`TopologyEvent`](vertex_swarm_topology::TopologyEvent) subscription.
    pub fn topology(&self) -> &TopologyHandle<Arc<Identity>> {
        &self.topology
    }

    /// Throttled client handle for chunk retrieval and upload.
    pub fn client(&self) -> &ClientHandle {
        &self.client
    }

    /// The shared client accounting (selector, throttle, forwarder, service,
    /// and settlement all read this instance).
    pub fn accounting(&self) -> &SharedAccounting {
        &self.accounting
    }

    /// The in-memory client chunk cache.
    pub fn store(&self) -> &Arc<ChunkStore<SystemClock>> {
        &self.store
    }

    /// The node's overlay address.
    pub fn overlay_address(&self) -> SwarmAddress {
        self.overlay
    }

    /// The node's libp2p peer id.
    pub fn local_peer_id(&self) -> PeerId {
        self.peer_id
    }
}
