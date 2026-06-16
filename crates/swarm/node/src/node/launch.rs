//! Fluent launcher for an embedded Swarm client node.
//!
//! [`ClientLauncher`] is the lightweight entry point shared by native
//! embedders and the browser client: no database, no chain provider, no RPC
//! server. It composes a [`ClientNode`] from an identity and a handful of
//! network knobs, spawns the node run loop, the client service, and the
//! peer-manager tick on the current [`TaskExecutor`], and hands back a
//! [`LaunchedClient`] with the handles a caller needs to observe the topology
//! and issue chunk reads and writes. The full native stack (storage, chain,
//! settlement, RPC) goes through `vertex-swarm-builder` instead.

use std::{net::IpAddr, sync::Arc, time::Duration};

use eyre::{Result, WrapErr};
use libp2p::{Multiaddr, PeerId};
use nectar_primitives::SwarmAddress;
use vertex_swarm_api::{
    DefaultPeerConfig, SwarmIdentity, SwarmNetworkConfig, SwarmPeerConfig, SwarmRoutingConfig,
};
use vertex_swarm_bandwidth::{AccountingBuilder, DefaultBandwidthConfig};
use vertex_swarm_peer_manager::{DEFAULT_TICK_INTERVAL, spawn_peer_manager_task};
use vertex_swarm_spec::HasSpec;
use vertex_swarm_topology::{KademliaConfig, TopologyHandle};
use vertex_tasks::TaskExecutor;

use super::client::{ClientNode, ObservedAddr};
use crate::{ClientHandle, SelfThrottle};

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
pub struct ClientLauncher<I: SwarmIdentity + Clone> {
    identity: I,
    bootnodes: Vec<Multiaddr>,
    kademlia: KademliaConfig,
    max_peers: usize,
    idle_timeout: Duration,
    /// Outbound self-throttle, on by default.
    throttle: bool,
    /// Bandwidth config the self-throttle and pricer read from.
    bandwidth: DefaultBandwidthConfig,
}

impl<I: SwarmIdentity + Clone> ClientLauncher<I> {
    /// Create a launcher for the given identity with default settings.
    #[must_use]
    pub fn new(identity: I) -> Self {
        Self {
            identity,
            bootnodes: Vec::new(),
            kademlia: KademliaConfig::default(),
            max_peers: DEFAULT_MAX_PEERS,
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            throttle: true,
            bandwidth: DefaultBandwidthConfig::default(),
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

    /// Set the transport-layer cap on established connections.
    #[must_use]
    pub fn with_max_peers(mut self, max: usize) -> Self {
        self.max_peers = max;
        self
    }

    /// Disable the outbound self-throttle (on by default).
    #[must_use]
    pub fn without_throttle(mut self) -> Self {
        self.throttle = false;
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
    /// natively, secure websockets in the browser), spawns the node run loop,
    /// the client service, and the peer-manager tick on the current
    /// [`TaskExecutor`], and returns a [`LaunchedClient`]. The node dials its
    /// bootnodes as part of startup; from there the Kademlia routing table
    /// fills on its own.
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
    pub async fn launch(self) -> Result<LaunchedClient<I>>
    where
        I: HasSpec,
    {
        let config = LaunchNetworkConfig {
            bootnodes: self.bootnodes,
            peer: DefaultPeerConfig::default(),
            routing: self.kademlia.clone(),
            max_peers: self.max_peers,
            idle_timeout: self.idle_timeout,
        };

        let (node, client_service, client_handle) = ClientNode::builder(self.identity.clone())
            .with_kademlia_config(self.kademlia)
            .build(&config, None)
            .await
            .wrap_err("failed to build client node")?;

        let topology = node.topology_handle().clone();
        let overlay = node.overlay_address();
        let peer_id = *node.local_peer_id();
        // Reader for our externally-observed address (our public IP), populated
        // by the run loop from identify. Grab it before the node is moved into
        // the spawned loop.
        let observed_addr = node.observed_addr_cell();

        // Outbound self-throttle (default on). The lightweight launcher carries
        // no accounting stack of its own, so build a minimal client-accounting
        // instance purely to pace against: a balance tracker with no settlement
        // providers plus the pricer baked into the bandwidth config (the same
        // proximity pricer the network meters chunk transfers by). The throttle
        // reads each peer's pseudosettle allowance off that accounting and its
        // forgiveness rate / safety margin off the bandwidth config, so a burst
        // never crosses the remote's settlement trigger. Both the returned
        // handle and the service must share the *same* throttle so a peer
        // disconnect clears the bucket the outbound API paces against.
        let (client_handle, client_service) = if self.throttle {
            let accounting = AccountingBuilder::new(self.bandwidth.clone())
                .with_pricer_from_config(HasSpec::spec(&self.identity).clone())
                .build(&self.identity);
            let throttle = Arc::new(SelfThrottle::new(&accounting, &self.bandwidth));
            (
                client_handle.with_throttle(Arc::clone(&throttle)),
                client_service.with_throttle(throttle),
            )
        } else {
            (client_handle, client_service)
        };

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
        executor.spawn_service("swarm.client_service", client_service);

        // The node run loop owns the libp2p swarm. It starts listening (a
        // no-op for a dial-only client) and then dials bootnodes and services
        // the event loop for the session.
        spawn_node_run_loop(&executor, node);

        Ok(LaunchedClient {
            topology,
            client: client_handle,
            overlay,
            peer_id,
            observed_addr,
        })
    }
}

/// Spawn the node run loop on the executor.
///
/// The native swarm and its futures are `Send`, so the run loop spawns as a
/// critical task on the tokio runtime and participates in graceful shutdown.
#[cfg(not(target_arch = "wasm32"))]
fn spawn_node_run_loop<I: SwarmIdentity + Clone>(executor: &TaskExecutor, node: ClientNode<I>) {
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
fn spawn_node_run_loop<I: SwarmIdentity + Clone>(executor: &TaskExecutor, node: ClientNode<I>) {
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
pub struct LaunchedClient<I: SwarmIdentity> {
    topology: TopologyHandle<I>,
    client: ClientHandle,
    overlay: SwarmAddress,
    peer_id: PeerId,
    /// Reader for our externally-observed address, populated from identify.
    observed_addr: ObservedAddr,
}

impl<I: SwarmIdentity> LaunchedClient<I> {
    /// Topology handle for readiness polling and
    /// [`TopologyEvent`](vertex_swarm_topology::TopologyEvent) subscription.
    pub fn topology(&self) -> &TopologyHandle<I> {
        &self.topology
    }

    /// Client handle for chunk retrieval and upload.
    pub fn client(&self) -> &ClientHandle {
        &self.client
    }

    /// The node's overlay address.
    pub fn overlay_address(&self) -> SwarmAddress {
        self.overlay
    }

    /// The node's libp2p peer id.
    pub fn local_peer_id(&self) -> PeerId {
        self.peer_id
    }

    /// Our externally-observed address (public IP), learned from identify.
    pub fn observed_external_addr(&self) -> Option<Multiaddr> {
        self.observed_addr.addr()
    }

    /// The IP component of [`observed_external_addr`](Self::observed_external_addr).
    pub fn observed_external_ip(&self) -> Option<IpAddr> {
        self.observed_addr.ip()
    }
}
