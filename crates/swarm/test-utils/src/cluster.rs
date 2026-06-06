//! In-process multi-node cluster harness for integration tests.
//!
//! Spins up one [`vertex_swarm_node::BootNode`] and N
//! [`vertex_swarm_node::ClientNode`]s on the local loopback interface with
//! hermetic networking (ephemeral TCP ports, in-memory peer stores, isolated
//! `network_id`). Each node owns its own libp2p swarm and event loop; the
//! harness exposes [`vertex_swarm_topology::TopologyHandle`]s and bookkeeping
//! ([`PeerId`], listen [`Multiaddr`]) so tests can subscribe to
//! [`vertex_swarm_topology::TopologyEvent`]s without poking at private state.
//!
//! # Design constraints
//!
//! - **Persistent identity.** Each node is constructed from a single
//!   [`Identity`] generated up front and cloned into the builder. This
//!   models a bootnode whose overlay address survives restarts.
//! - **TCP transport.** The current `BootNodeBuilder` / `ClientNodeBuilder`
//!   hardwire `libp2p::tcp::tokio::Transport` (see
//!   `vertex_swarm_node::node::builder::build_base_node`). Hermetic memory
//!   transport would require a public builder hook that does not exist in
//!   `main` yet — the workaround is to bind on `127.0.0.1` with
//!   OS-assigned ports.
//! - **Wall-clock timeouts.** Because real TCP I/O is involved, integration
//!   tests use bounded `tokio::time::timeout` rather than
//!   `tokio::time::pause()`; pause would freeze timers without freezing the
//!   OS network stack. Total runtime is bounded by the test (the cluster
//!   test caps itself well under 30 seconds).
//!
//! # Example
//!
//! ```ignore
//! use vertex_swarm_test_utils::cluster::ClusterBuilder;
//!
//! # async fn doc() -> eyre::Result<()> {
//! let cluster = ClusterBuilder::new()
//!     .with_bootnode()
//!     .with_clients(2)
//!     .build()
//!     .await?;
//!
//! let bootnode_topo = cluster.bootnode().topology.clone();
//! assert!(bootnode_topo.connected_peers_count() <= 2);
//! cluster.shutdown().await;
//! # Ok(()) }
//! ```

use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

use eyre::{Result, WrapErr};
use libp2p::{Multiaddr, PeerId};
use nectar_primitives::SwarmAddress;
use tokio::task::JoinHandle;
use vertex_swarm_api::{SwarmIdentity, SwarmNodeType};
use vertex_swarm_identity::Identity;
use vertex_swarm_spec::Spec;
use vertex_tasks::TaskManager;

use crate::spec::TEST_NETWORK_ID;

/// Role a node plays inside the cluster.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum NodeRole {
    /// Bootnode: topology-only behaviour.
    Bootnode,
    /// Client: topology + client protocols.
    Client,
}

/// Live handle to a cluster node.
///
/// All fields are immutable snapshots captured at startup time except
/// [`topology`](Self::topology), which is a clonable handle that reflects
/// live state.
pub struct ClusterNodeHandle {
    /// Role this node was constructed with.
    pub role: NodeRole,
    /// Overlay address (stable for the lifetime of the cluster).
    pub overlay: SwarmAddress,
    /// libp2p peer id (stable for the lifetime of the cluster).
    pub peer_id: PeerId,
    /// Loopback listen multiaddr including `/p2p/<peer_id>`.
    pub listen_addr: Multiaddr,
    /// Live topology handle (clonable; queries reflect current state).
    pub topology: vertex_swarm_topology::TopologyHandle<Identity>,
    /// Join handle for the spawned run loop (taken at shutdown).
    join: Option<JoinHandle<Result<()>>>,
}

impl std::fmt::Debug for ClusterNodeHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClusterNodeHandle")
            .field("role", &self.role)
            .field("overlay", &self.overlay)
            .field("peer_id", &self.peer_id)
            .field("listen_addr", &self.listen_addr)
            .finish_non_exhaustive()
    }
}

/// Builder for the in-process cluster.
pub struct ClusterBuilder {
    spec: Arc<Spec>,
    has_bootnode: bool,
    client_count: usize,
}

impl Default for ClusterBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ClusterBuilder {
    /// Start a fresh cluster blueprint on the isolated test network.
    ///
    /// The default spec has *no* bootnodes baked in, so each node only ever
    /// dials addresses we hand it explicitly. `test_spec_isolated()` would
    /// inherit testnet's dnsaddr bootnodes — dialling real testnet bootnodes
    /// from an integration test is both flaky and slow.
    pub fn new() -> Self {
        let spec = Arc::new(
            vertex_swarm_spec::SpecBuilder::testnet()
                .network_id(TEST_NETWORK_ID)
                .bootnodes(Vec::new())
                .build(),
        );
        Self {
            spec,
            has_bootnode: false,
            client_count: 0,
        }
    }

    /// Use a custom [`Spec`] (e.g. real testnet network id).
    pub fn with_spec(mut self, spec: Arc<Spec>) -> Self {
        self.spec = spec;
        self
    }

    /// Add exactly one bootnode to the cluster. Calling this twice has no
    /// extra effect; the bootnode overlay is well-known by construction.
    pub fn with_bootnode(mut self) -> Self {
        self.has_bootnode = true;
        self
    }

    /// Add `n` client nodes to the cluster.
    pub fn with_clients(mut self, n: usize) -> Self {
        self.client_count = n;
        self
    }

    /// Build and start the cluster.
    ///
    /// Each node binds to `127.0.0.1` on an OS-assigned ephemeral port and
    /// begins its event loop. Client nodes are pre-configured with the
    /// bootnode's listen multiaddr as their sole bootstrap entry.
    ///
    /// Requires an active Tokio runtime; installs a [`TaskManager`] if one
    /// is not already current.
    pub async fn build(self) -> Result<Cluster> {
        // The topology stack expects a global TaskExecutor; install one if
        // the test process has not already done so. The handle is held by
        // the returned [`Cluster`] so the executor outlives the nodes.
        let task_manager = match vertex_tasks::TaskExecutor::try_current() {
            Ok(_) => None,
            Err(_) => Some(TaskManager::current()),
        };

        // Reserve every port upfront, holding the placeholder TcpListeners
        // across the whole build. This makes the harness safe against
        // itself: by the time we drop placeholder N to start node N's
        // libp2p listener, every other node's placeholder is still held,
        // so a sibling cannot reuse the same OS-assigned port. The
        // residual race is only against external processes.
        let total_nodes = usize::from(self.has_bootnode) + self.client_count;
        let mut reservations: Vec<PortReservation> = Vec::with_capacity(total_nodes);
        for _ in 0..total_nodes {
            reservations.push(reserve_ephemeral_port()?);
        }
        let mut reservations = reservations.into_iter();

        let mut bootnode = None;
        let mut bootnode_addrs: Vec<Multiaddr> = Vec::new();

        if self.has_bootnode {
            let identity = persistent_identity(&self.spec, SwarmNodeType::Bootnode);
            let reservation = reservations.next().expect("one port per node");
            let listen_addr = reservation.listen_addr()?;
            // Drop the placeholder immediately before libp2p binds the port.
            drop(reservation);
            let handle = spawn_bootnode(identity, listen_addr, &[]).await?;
            bootnode_addrs.push(handle.listen_addr.clone());
            bootnode = Some(handle);
        }

        let mut clients = Vec::with_capacity(self.client_count);
        for _ in 0..self.client_count {
            let identity = persistent_identity(&self.spec, SwarmNodeType::Client);
            let reservation = reservations.next().expect("one port per node");
            let listen_addr = reservation.listen_addr()?;
            drop(reservation);
            let handle = spawn_client(identity, listen_addr, &bootnode_addrs).await?;
            clients.push(handle);
        }

        // Subscribe to the bootnode's topology event stream BEFORE kicking
        // any dial: on fast loopback both client handshakes can complete in
        // the window between `build` returning and a caller calling
        // `subscribe`, and `broadcast::Sender::subscribe` does not replay
        // past events. Stash the receiver so the caller can read it via
        // [`Cluster::bootnode_events`].
        let bootnode_events = bootnode.as_ref().map(|bn| bn.topology.subscribe());

        // The production builder issues `ConnectBootnodes` *before* libp2p
        // has observed its own listen addresses, so the first dial trips
        // the "capability unknown → no reachable addresses" guard in
        // `vertex_net_local::is_dialable`. Re-issue the command until at
        // least one peer is connected on each client; idempotent because
        // already-connected peers are skipped via `is_peer_tracked`.
        kick_bootnode_dial(&clients).await;

        Ok(Cluster {
            bootnode,
            bootnode_events,
            clients,
            task_manager,
        })
    }
}

/// Running cluster handle.
pub struct Cluster {
    bootnode: Option<ClusterNodeHandle>,
    /// Receiver subscribed inside [`ClusterBuilder::build`] BEFORE any dial
    /// is kicked. Callers should consume this rather than calling
    /// `bootnode().topology.subscribe()` themselves, which races with
    /// handshake completion on fast loopback.
    bootnode_events: Option<tokio::sync::broadcast::Receiver<vertex_swarm_topology::TopologyEvent>>,
    clients: Vec<ClusterNodeHandle>,
    /// Held to keep the global executor alive for the duration of the test
    /// and consumed by [`Cluster::shutdown`] to fire the [`Shutdown`] signal.
    /// `None` when the test runtime already installed a [`TaskManager`]
    /// before the cluster was built.
    ///
    /// [`Shutdown`]: vertex_tasks::Shutdown
    task_manager: Option<TaskManager>,
}

impl Cluster {
    /// Get a reference to the bootnode handle.
    ///
    /// # Panics
    /// Panics if [`ClusterBuilder::with_bootnode`] was not called.
    #[allow(clippy::expect_used)]
    pub fn bootnode(&self) -> &ClusterNodeHandle {
        self.bootnode
            .as_ref()
            .expect("cluster constructed without a bootnode")
    }

    /// Get a reference to the client handles.
    pub fn clients(&self) -> &[ClusterNodeHandle] {
        &self.clients
    }

    /// Number of clients in the cluster.
    pub fn client_count(&self) -> usize {
        self.clients.len()
    }

    /// Take the pre-subscribed bootnode topology event receiver. Subscription
    /// happens inside [`ClusterBuilder::build`] before any dial is kicked,
    /// so the caller is guaranteed to see every event the bootnode emits.
    /// Returns `None` if the cluster has no bootnode or if the receiver has
    /// already been taken.
    pub fn take_bootnode_events(
        &mut self,
    ) -> Option<tokio::sync::broadcast::Receiver<vertex_swarm_topology::TopologyEvent>> {
        self.bootnode_events.take()
    }

    /// Initiate graceful shutdown of every node, then await each task.
    ///
    /// Best-effort: individual join errors are returned alongside successful
    /// completions so the caller can choose whether to assert.
    pub async fn shutdown(mut self) -> Vec<Result<()>> {
        // Fire the global shutdown signal. Two cases:
        //
        // - If the cluster installed its *own* [`TaskManager`] (no executor
        //   was current before [`ClusterBuilder::build`]), we own it and can
        //   call [`TaskManager::graceful_shutdown`] directly. That consumes
        //   the manager and drops its `Signal`, which fires the
        //   `Shutdown` future every node holds. Because that call blocks
        //   on a `Condvar`, we run it on the blocking pool.
        //
        // - If the manager was created elsewhere (typical inside
        //   `#[tokio::test]` that wraps `cluster::build`), we can only
        //   signal via [`TaskExecutor::initiate_graceful_shutdown`], which
        //   relies on the outer manager being polled. The integration test
        //   in `tests/bootnode_cluster.rs` is structured to drop the
        //   `Cluster` (and therefore its `TaskManager`, if any) before
        //   `#[tokio::test]` tears the runtime down, so this branch is
        //   safe in practice.
        if let Some(manager) = self.task_manager.take() {
            tokio::task::spawn_blocking(move || {
                // Bounded so a stuck node does not wedge the test runner.
                manager.graceful_shutdown_with_timeout(Duration::from_secs(15));
            })
            .await
            .ok();
        } else if let Ok(executor) = vertex_tasks::TaskExecutor::try_current() {
            let _ = executor.initiate_graceful_shutdown();
        }

        let mut joins = Vec::new();
        if let Some(mut bn) = self.bootnode.take()
            && let Some(handle) = bn.join.take()
        {
            joins.push(handle);
        }
        for mut client in self.clients.drain(..) {
            if let Some(handle) = client.join.take() {
                joins.push(handle);
            }
        }

        let mut results = Vec::with_capacity(joins.len());
        for handle in joins {
            // Bound the await so a stuck event loop does not hang the test.
            match tokio::time::timeout(Duration::from_secs(5), handle).await {
                Ok(Ok(inner)) => results.push(inner),
                Ok(Err(join_err)) => results.push(Err(eyre::eyre!(
                    "cluster node task panicked or was cancelled: {join_err}"
                ))),
                Err(_) => results.push(Err(eyre::eyre!(
                    "cluster node task did not terminate within the shutdown deadline"
                ))),
            }
        }
        results
    }
}

/// Construct an [`Identity`] that is reused across the test cluster lifetime.
///
/// Uses [`Identity::new`] (the persistent constructor) so the bootnode
/// builder's `assert_persistent_identity` check passes; the signer and
/// nonce are random per cluster instance but stable for the cluster's
/// lifetime, which mirrors how a keystore-backed identity looks from the
/// rest of the topology stack.
fn persistent_identity(spec: &Arc<Spec>, node_type: SwarmNodeType) -> Identity {
    use alloy_signer_local::LocalSigner;
    use vertex_swarm_primitives::Nonce;
    Identity::new(
        LocalSigner::random(),
        Nonce::random(),
        Arc::clone(spec),
        node_type,
    )
}

/// Workaround for the bootnode race in `vertex_swarm_node::node::builder`.
///
/// Each client's first `ConnectBootnodes` command races against the libp2p
/// swarm emitting its own `NewListenAddr` events; until at least one listen
/// address has been observed, `vertex_net_local::is_dialable` rejects every
/// candidate address. The production builder issues `ConnectBootnodes` from
/// inside `build_base_node`, before the swarm has been polled, so the first
/// dial fails silently. We mitigate by re-sending the command after a short
/// yield; the underlying dial path is idempotent.
async fn kick_bootnode_dial(clients: &[ClusterNodeHandle]) {
    use vertex_swarm_api::{SwarmTopologyCommands as _, SwarmTopologyStats as _};

    // Re-issue `connect_bootnodes` until each client is actually connected to
    // a peer, bounded by a real timeout. The initial `connect_bootnodes` call
    // in the production builder races with libp2p's first `NewListenAddr`
    // emission — if it fires first, `vertex_net_local::is_dialable` rejects
    // the candidate and the dial dies silently. Polling for the observable
    // outcome (peer count > 0) is faster on the common path than the old
    // fixed sleep and remains correct under load.
    const POLL_BUDGET: Duration = Duration::from_secs(2);
    const POLL_INTERVAL: Duration = Duration::from_millis(25);
    let deadline = tokio::time::Instant::now() + POLL_BUDGET;
    loop {
        for client in clients {
            let _ = client.topology.connect_bootnodes().await;
        }
        if clients
            .iter()
            .all(|c| c.topology.connected_peers_count() > 0)
        {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            // Best-effort: any remaining missing handshakes will surface
            // downstream as a clearer test failure.
            return;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Holds the placeholder [`TcpListener`] for a reserved ephemeral port so the
/// port cannot be reassigned to a sibling node in the same harness. The
/// caller drops the reservation immediately before libp2p binds the port;
/// only that one-call window is racy, and only against external processes.
struct PortReservation {
    listener: TcpListener,
}

impl PortReservation {
    fn listen_addr(&self) -> Result<Multiaddr> {
        let port = self
            .listener
            .local_addr()
            .wrap_err("ephemeral TCP socket has no local address")?
            .port();
        format!("/ip4/127.0.0.1/tcp/{port}")
            .parse()
            .wrap_err("listen multiaddr is well-formed")
    }
}

fn reserve_ephemeral_port() -> Result<PortReservation> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .wrap_err("failed to bind ephemeral TCP port on loopback")?;
    Ok(PortReservation { listener })
}

async fn spawn_bootnode(
    identity: Identity,
    listen_addr: Multiaddr,
    bootnodes: &[Multiaddr],
) -> Result<ClusterNodeHandle> {
    use vertex_swarm_node::BootNode;

    let network_config = TestNetworkConfig::new(vec![listen_addr.clone()], bootnodes.to_vec());

    let overlay = identity.overlay_address();
    let mut bootnode = BootNode::builder(identity)
        .build(&network_config, None, None)
        .await
        .wrap_err("failed to build bootnode")?;
    bootnode
        .start_listening()
        .wrap_err("bootnode failed to start listening")?;

    let peer_id = *bootnode.local_peer_id();
    let topology = bootnode.topology_handle().clone();
    let listen_with_peer = listen_addr.with(libp2p::multiaddr::Protocol::P2p(peer_id));

    let join = spawn_node_task("cluster-bootnode", move |graceful| async move {
        bootnode.run(graceful).await
    });

    Ok(ClusterNodeHandle {
        role: NodeRole::Bootnode,
        overlay,
        peer_id,
        listen_addr: listen_with_peer,
        topology,
        join: Some(join),
    })
}

async fn spawn_client(
    identity: Identity,
    listen_addr: Multiaddr,
    bootnodes: &[Multiaddr],
) -> Result<ClusterNodeHandle> {
    use vertex_swarm_node::ClientNode;

    let network_config = TestNetworkConfig::new(vec![listen_addr.clone()], bootnodes.to_vec());

    let overlay = identity.overlay_address();
    let (mut client, _service, _handle) = ClientNode::builder(identity)
        .build(&network_config, None, None)
        .await
        .wrap_err("failed to build client node")?;
    client
        .start_listening()
        .wrap_err("client failed to start listening")?;

    let peer_id = *client.local_peer_id();
    let topology = client.topology_handle().clone();
    let listen_with_peer = listen_addr.with(libp2p::multiaddr::Protocol::P2p(peer_id));

    let join = spawn_node_task("cluster-client", move |graceful| async move {
        client.run(graceful).await
    });

    Ok(ClusterNodeHandle {
        role: NodeRole::Client,
        overlay,
        peer_id,
        listen_addr: listen_with_peer,
        topology,
        join: Some(join),
    })
}

/// Spawn a node's `run` future on the executor with a graceful-shutdown
/// signal, and project its `Result<()>` out through a bridging task so we
/// can join on a typed handle.
fn spawn_node_task<F, Fut>(name: &'static str, f: F) -> JoinHandle<Result<()>>
where
    F: FnOnce(vertex_tasks::GracefulShutdown) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    let executor = vertex_tasks::TaskExecutor::current();
    let (result_tx, result_rx) = tokio::sync::oneshot::channel::<Result<()>>();

    let spawn = executor.spawn_with_graceful_shutdown_signal(name, move |graceful| async move {
        let res = f(graceful).await;
        // The receiver is held by the bridging task below — on send failure
        // the bridge has already been dropped (test aborted), so silently
        // discard the result.
        let _ = result_tx.send(res);
    });

    tokio::spawn(async move {
        // If the spawned task panicked, surface the panic message rather
        // than masking it as "task dropped without producing a result".
        match spawn.await {
            Ok(()) => {}
            Err(join_err) if join_err.is_panic() => {
                let payload = join_err.into_panic();
                let msg = payload
                    .downcast_ref::<&'static str>()
                    .map(|s| (*s).to_string())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "<non-string panic payload>".to_string());
                return Err(eyre::eyre!("{name} task panicked: {msg}"));
            }
            Err(join_err) => return Err(eyre::eyre!("{name} task join error: {join_err}")),
        }
        result_rx.await.unwrap_or_else(|_| {
            Err(eyre::eyre!(
                "{name} task dropped without producing a result"
            ))
        })
    })
}

/// Minimal in-memory implementation of the network/peer/routing config traits.
///
/// Mirrors the shape of `vertex_swarm_node::args::network::NetworkConfig` but
/// avoids pulling in the optional `cli` feature of `vertex-swarm-node`.
struct TestNetworkConfig {
    listen_addrs: Vec<Multiaddr>,
    bootnodes: Vec<Multiaddr>,
    trusted_peers: Vec<Multiaddr>,
    nat_addrs: Vec<Multiaddr>,
    peer: TestPeerConfig,
    routing: vertex_swarm_topology::KademliaConfig,
}

impl TestNetworkConfig {
    fn new(listen_addrs: Vec<Multiaddr>, bootnodes: Vec<Multiaddr>) -> Self {
        Self {
            listen_addrs,
            bootnodes,
            trusted_peers: Vec::new(),
            nat_addrs: Vec::new(),
            peer: TestPeerConfig,
            routing: vertex_swarm_topology::KademliaConfig::default(),
        }
    }
}

impl vertex_swarm_api::SwarmNetworkConfig for TestNetworkConfig {
    fn listen_addrs(&self) -> &[Multiaddr] {
        &self.listen_addrs
    }
    fn bootnodes(&self) -> &[Multiaddr] {
        &self.bootnodes
    }
    fn trusted_peers(&self) -> &[Multiaddr] {
        &self.trusted_peers
    }
    fn discovery_enabled(&self) -> bool {
        true
    }
    fn max_peers(&self) -> usize {
        16
    }
    fn idle_timeout(&self) -> Duration {
        Duration::from_secs(30)
    }
    fn nat_addrs(&self) -> &[Multiaddr] {
        &self.nat_addrs
    }
    fn nat_auto_enabled(&self) -> bool {
        false
    }
}

#[derive(Default)]
struct TestPeerConfig;

impl vertex_swarm_api::PeerConfigValues for TestPeerConfig {
    fn ban_threshold(&self) -> f64 {
        vertex_swarm_api::DEFAULT_PEER_BAN_THRESHOLD
    }
}

impl vertex_swarm_api::SwarmPeerConfig for TestNetworkConfig {
    type Peers = TestPeerConfig;
    fn peers(&self) -> &Self::Peers {
        &self.peer
    }
}

impl vertex_swarm_api::SwarmRoutingConfig for TestNetworkConfig {
    type Routing = vertex_swarm_topology::KademliaConfig;
    fn routing(&self) -> &Self::Routing {
        &self.routing
    }
}
