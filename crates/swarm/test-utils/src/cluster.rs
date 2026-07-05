//! In-process multi-node cluster harness for integration tests.
//!
//! Spins up one [`vertex_swarm_node::BootNode`] plus N
//! [`vertex_swarm_node::ClientNode`]s (and, behind the `cluster-storer`
//! feature, [`vertex_swarm_node::StorerNode`]s) sharing one process, each
//! owning its own libp2p swarm and event loop. The harness exposes
//! [`vertex_swarm_topology::TopologyHandle`]s and per-node bookkeeping
//! ([`PeerId`], listen [`Multiaddr`]) so tests can subscribe to
//! [`vertex_swarm_topology::TopologyEvent`]s without poking at private state.
//!
//! # Transport modes
//!
//! - [`Transport::Memory`] (the default) injects a channel-based memory
//!   transport into every node through the builders'
//!   [`with_transport`](vertex_swarm_node::ClientNodeBuilder::with_transport)
//!   seam. No OS sockets are bound, so a cluster scales to tens of nodes in
//!   process, and paused virtual time (`#[tokio::test(start_paused = true)]`)
//!   drives convergence: the transport wakes tasks through channels, never
//!   real timers, so an idle runtime auto-advances the clock. Memory addresses
//!   sidestep the dial-eligibility filter's IP-capability gate, so no
//!   bootnode-dial kick is needed.
//! - [`Transport::Tcp`] binds `127.0.0.1` on OS-assigned ports for the rare
//!   test that needs real sockets. Real I/O means wall-clock
//!   `tokio::time::timeout` rather than paused time, and the first
//!   `ConnectBootnodes` races the swarm's own `NewListenAddr`, so this mode
//!   re-issues the dial until each node connects.
//!
//! # Design constraints
//!
//! - **Persistent identity.** Each node is constructed from a single
//!   [`Identity`] generated up front. This models a bootnode whose overlay
//!   address survives restarts.
//! - **Per-node shutdown and panic surfacing.** Each node's `run` future is
//!   spawned on the [`TaskManager`], and a panic in one is surfaced as an
//!   `eyre` error out of [`Cluster::shutdown`] rather than lost.
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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use eyre::{Result, WrapErr};
use libp2p::{Multiaddr, PeerId};
use nectar_primitives::SwarmAddress;
use tokio::task::JoinHandle;
use vertex_swarm_api::{SwarmIdentity, SwarmNodeType};
use vertex_swarm_identity::Identity;
use vertex_swarm_node::TransportOverride;
use vertex_swarm_spec::Spec;
use vertex_tasks::TaskManager;

use crate::spec::TEST_NETWORK_ID;

/// Transport the cluster nodes run over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Transport {
    /// In-process channel-based memory transport. Hermetic and virtual-time
    /// friendly; the default.
    #[default]
    Memory,
    /// Real TCP on `127.0.0.1` with OS-assigned ports.
    Tcp,
}

/// Role a node plays inside the cluster.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum NodeRole {
    /// Bootnode: topology-only behaviour.
    Bootnode,
    /// Client: topology + client protocols.
    Client,
    /// Storer: topology + client + storer protocols.
    #[cfg(feature = "cluster-storer")]
    Storer,
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
    /// Listen multiaddr including `/p2p/<peer_id>`.
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
    transport: Transport,
    has_bootnode: bool,
    client_count: usize,
    #[cfg(feature = "cluster-storer")]
    storer_count: usize,
    max_peers: usize,
}

/// Default connection admission cap, generous enough for a ~50-node star.
const DEFAULT_MAX_PEERS: usize = 256;

impl Default for ClusterBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ClusterBuilder {
    /// Start a fresh cluster blueprint on the isolated test network over the
    /// default [`Transport::Memory`].
    ///
    /// The default spec has *no* bootnodes baked in, so each node only ever
    /// dials addresses we hand it explicitly. `test_spec_isolated()` would
    /// inherit testnet's dnsaddr bootnodes, and dialling real testnet
    /// bootnodes from an integration test is both flaky and slow.
    pub fn new() -> Self {
        let spec = Arc::new(
            vertex_swarm_spec::SpecBuilder::testnet()
                .network_id(TEST_NETWORK_ID)
                .bootnodes(Vec::new())
                .build(),
        );
        Self {
            spec,
            transport: Transport::default(),
            has_bootnode: false,
            client_count: 0,
            #[cfg(feature = "cluster-storer")]
            storer_count: 0,
            max_peers: DEFAULT_MAX_PEERS,
        }
    }

    /// Use a custom [`Spec`] (e.g. real testnet network id).
    pub fn with_spec(mut self, spec: Arc<Spec>) -> Self {
        self.spec = spec;
        self
    }

    /// Select the transport the nodes run over. Defaults to
    /// [`Transport::Memory`].
    pub fn with_transport_mode(mut self, transport: Transport) -> Self {
        self.transport = transport;
        self
    }

    /// Override the per-node connection admission cap.
    pub fn with_max_peers(mut self, max_peers: usize) -> Self {
        self.max_peers = max_peers;
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

    /// Add `n` storer nodes to the cluster.
    #[cfg(feature = "cluster-storer")]
    pub fn with_storers(mut self, n: usize) -> Self {
        self.storer_count = n;
        self
    }

    /// Build and start the cluster.
    ///
    /// Each node begins its event loop with the bootnode's listen multiaddr as
    /// its sole bootstrap entry. Requires an active Tokio runtime; installs a
    /// [`TaskManager`] if one is not already current.
    pub async fn build(self) -> Result<Cluster> {
        // The topology stack expects a global TaskExecutor; install one if the
        // test process has not already done so. The handle is held by the
        // returned [`Cluster`] so the executor outlives the nodes.
        let task_manager = match vertex_tasks::TaskExecutor::try_current() {
            Ok(_) => None,
            Err(_) => Some(TaskManager::current()),
        };

        let mut bootnode = None;
        let mut bootnode_addrs: Vec<Multiaddr> = Vec::new();

        if self.has_bootnode {
            let identity = persistent_identity(&self.spec, SwarmNodeType::Bootnode);
            let listen = self.reserve_listen_addr()?;
            let handle =
                spawn_bootnode(identity, listen, &[], self.transport, self.max_peers).await?;
            bootnode_addrs.push(handle.listen_addr.clone());
            bootnode = Some(handle);
        }

        // Subscribe to the bootnode's topology stream BEFORE any peer is
        // spawned: the earliest a `PeerReady` can fire is when a client
        // connects, and no client exists yet, so this cannot miss an event.
        // `broadcast::Sender::subscribe` does not replay past events.
        let bootnode_events = bootnode.as_ref().map(|bn| bn.topology.subscribe());

        let mut clients = Vec::with_capacity(self.client_count);
        for _ in 0..self.client_count {
            let identity = persistent_identity(&self.spec, SwarmNodeType::Client);
            let listen = self.reserve_listen_addr()?;
            let handle = spawn_client(
                identity,
                listen,
                &bootnode_addrs,
                self.transport,
                self.max_peers,
            )
            .await?;
            clients.push(handle);
        }

        #[cfg(feature = "cluster-storer")]
        let mut storers = Vec::with_capacity(self.storer_count);
        #[cfg(feature = "cluster-storer")]
        for _ in 0..self.storer_count {
            let identity = persistent_identity(&self.spec, SwarmNodeType::Storer);
            let listen = self.reserve_listen_addr()?;
            let handle = spawn_storer(
                identity,
                listen,
                &bootnode_addrs,
                self.transport,
                self.max_peers,
            )
            .await?;
            storers.push(handle);
        }

        // Under real TCP the production builder issues `ConnectBootnodes`
        // before libp2p has observed its own listen addresses, so the first
        // dial trips the "capability unknown -> no reachable addresses" guard
        // in `vertex_net_local::is_dialable`. Re-issue until each node
        // connects. Memory addresses carry no IP, so that guard never trips and
        // no kick is required.
        if self.transport == Transport::Tcp {
            #[cfg(feature = "cluster-storer")]
            let dialers: Vec<&ClusterNodeHandle> = clients.iter().chain(storers.iter()).collect();
            #[cfg(not(feature = "cluster-storer"))]
            let dialers: Vec<&ClusterNodeHandle> = clients.iter().collect();
            kick_bootnode_dial(&dialers).await;
        }

        Ok(Cluster {
            bootnode,
            bootnode_events,
            clients,
            #[cfg(feature = "cluster-storer")]
            storers,
            task_manager,
        })
    }

    /// Allocate the listen multiaddr for one node under the selected transport.
    ///
    /// Memory nodes take a process-unique `/memory/<port>` picked up front so
    /// the address (with the peer id appended after build) can seed clients
    /// before the node's loop is polled. TCP nodes reserve an ephemeral
    /// loopback port, held until libp2p binds it.
    fn reserve_listen_addr(&self) -> Result<ListenReservation> {
        match self.transport {
            Transport::Memory => Ok(ListenReservation::Memory(next_memory_addr())),
            Transport::Tcp => reserve_ephemeral_port().map(ListenReservation::Tcp),
        }
    }
}

/// A reserved listen multiaddr, resolved just before the node binds it.
enum ListenReservation {
    /// A process-unique `/memory/<port>` address.
    Memory(Multiaddr),
    /// A placeholder [`TcpListener`] holding an ephemeral loopback port so a
    /// sibling in the same harness cannot reuse it.
    Tcp(TcpListener),
}

impl ListenReservation {
    /// The multiaddr the node should listen on, releasing any placeholder
    /// socket so libp2p can bind it.
    fn into_listen_addr(self) -> Result<Multiaddr> {
        match self {
            Self::Memory(addr) => Ok(addr),
            Self::Tcp(listener) => {
                let port = listener
                    .local_addr()
                    .wrap_err("ephemeral TCP socket has no local address")?
                    .port();
                // Drop the placeholder immediately before libp2p binds the
                // port; only that one-call window is racy, and only against
                // external processes.
                drop(listener);
                format!("/ip4/127.0.0.1/tcp/{port}")
                    .parse()
                    .wrap_err("listen multiaddr is well-formed")
            }
        }
    }
}

/// Running cluster handle.
pub struct Cluster {
    bootnode: Option<ClusterNodeHandle>,
    /// Receiver subscribed inside [`ClusterBuilder::build`] before any peer is
    /// spawned. Callers should consume this rather than calling
    /// `bootnode().topology.subscribe()` themselves, which races with
    /// handshake completion.
    bootnode_events: Option<tokio::sync::broadcast::Receiver<vertex_swarm_topology::TopologyEvent>>,
    clients: Vec<ClusterNodeHandle>,
    #[cfg(feature = "cluster-storer")]
    storers: Vec<ClusterNodeHandle>,
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

    /// Get a reference to the storer handles.
    #[cfg(feature = "cluster-storer")]
    pub fn storers(&self) -> &[ClusterNodeHandle] {
        &self.storers
    }

    /// Number of storers in the cluster.
    #[cfg(feature = "cluster-storer")]
    pub fn storer_count(&self) -> usize {
        self.storers.len()
    }

    /// Take the pre-subscribed bootnode topology event receiver. Subscription
    /// happens inside [`ClusterBuilder::build`] before any peer is spawned, so
    /// the caller is guaranteed to see every event the bootnode emits. Returns
    /// `None` if the cluster has no bootnode or if the receiver has already
    /// been taken.
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
        //   the manager and drops its `Signal`, which fires the `Shutdown`
        //   future every node holds. Because that call blocks on a `Condvar`,
        //   we run it on the blocking pool.
        //
        // - If the manager was created elsewhere (typical inside a
        //   `#[tokio::test]` that wraps `cluster::build`), we can only signal
        //   via [`TaskExecutor::initiate_graceful_shutdown`], which relies on
        //   the outer manager being polled.
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
        #[cfg(feature = "cluster-storer")]
        for mut storer in self.storers.drain(..) {
            if let Some(handle) = storer.join.take() {
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
/// builder's `assert_persistent_identity` check passes; the signer and nonce
/// are random per cluster instance but stable for the cluster's lifetime.
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

/// Process-unique memory port allocator.
///
/// Seeded once with a random high base so concurrent clusters, and the
/// swarm-test harness that listens on `/memory/0`, never collide on a port
/// within one process.
static NEXT_MEMORY_PORT: LazyLock<AtomicU64> =
    LazyLock::new(|| AtomicU64::new(u64::from(rand::random::<u32>()) + 1));

fn next_memory_addr() -> Multiaddr {
    let port = NEXT_MEMORY_PORT.fetch_add(1, Ordering::Relaxed);
    format!("/memory/{port}")
        .parse()
        .expect("literal /memory addr is well-formed")
}

/// A memory [`TransportOverride`] for one node: an authenticated, multiplexed
/// channel transport in place of the default TCP stack.
fn memory_transport() -> TransportOverride {
    Box::new(|keypair: &libp2p::identity::Keypair| {
        use libp2p::Transport as _;
        use libp2p::core::transport::MemoryTransport;
        use libp2p::core::upgrade::Version;
        use libp2p::{noise, yamux};

        let transport = MemoryTransport::default()
            .upgrade(Version::V1)
            .authenticate(noise::Config::new(keypair)?)
            .multiplex(yamux::Config::default())
            .boxed();
        Ok(transport)
    })
}

/// The transport override for a given mode: memory injects a channel transport,
/// TCP leaves the default stack in place.
fn transport_override(transport: Transport) -> Option<TransportOverride> {
    match transport {
        Transport::Memory => Some(memory_transport()),
        Transport::Tcp => None,
    }
}

/// Re-issue `connect_bootnodes` on each dialer until it is connected, bounded
/// by a real timeout. Only used under [`Transport::Tcp`]: the first
/// `connect_bootnodes` races libp2p's first `NewListenAddr`, and if it wins,
/// `vertex_net_local::is_dialable` rejects the candidate because the node's IP
/// capability is still unknown, so the dial dies silently.
async fn kick_bootnode_dial(dialers: &[&ClusterNodeHandle]) {
    use vertex_swarm_api::{SwarmTopologyCommands as _, SwarmTopologyStats as _};

    const POLL_BUDGET: Duration = Duration::from_secs(2);
    const POLL_INTERVAL: Duration = Duration::from_millis(25);
    let deadline = tokio::time::Instant::now() + POLL_BUDGET;
    loop {
        for dialer in dialers {
            let _ = dialer.topology.connect_bootnodes().await;
        }
        if dialers
            .iter()
            .all(|d| d.topology.connected_peers_count() > 0)
        {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            // Best-effort: any remaining missing handshakes surface downstream
            // as a clearer test failure.
            return;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

fn reserve_ephemeral_port() -> Result<TcpListener> {
    TcpListener::bind("127.0.0.1:0").wrap_err("failed to bind ephemeral TCP port on loopback")
}

async fn spawn_bootnode(
    identity: Identity,
    listen: ListenReservation,
    bootnodes: &[Multiaddr],
    transport: Transport,
    max_peers: usize,
) -> Result<ClusterNodeHandle> {
    use vertex_swarm_node::BootNode;

    let listen_addr = listen.into_listen_addr()?;
    let network_config =
        TestNetworkConfig::new(vec![listen_addr.clone()], bootnodes.to_vec(), max_peers);

    let overlay = identity.overlay_address();
    let mut builder = BootNode::builder(identity);
    if let Some(override_fn) = transport_override(transport) {
        builder = builder.with_transport(override_fn);
    }
    let mut bootnode = builder
        .build(&network_config, None)
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
    listen: ListenReservation,
    bootnodes: &[Multiaddr],
    transport: Transport,
    max_peers: usize,
) -> Result<ClusterNodeHandle> {
    use vertex_swarm_node::ClientNode;

    let listen_addr = listen.into_listen_addr()?;
    let network_config =
        TestNetworkConfig::new(vec![listen_addr.clone()], bootnodes.to_vec(), max_peers);

    let overlay = identity.overlay_address();
    let mut builder = ClientNode::builder(identity);
    if let Some(override_fn) = transport_override(transport) {
        builder = builder.with_transport(override_fn);
    }
    let (mut client, _service, _handle) = builder
        .build(&network_config, None)
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

#[cfg(feature = "cluster-storer")]
async fn spawn_storer(
    identity: Identity,
    listen: ListenReservation,
    bootnodes: &[Multiaddr],
    transport: Transport,
    max_peers: usize,
) -> Result<ClusterNodeHandle> {
    use vertex_swarm_node::StorerNode;

    use crate::storage::MockStorage;

    let listen_addr = listen.into_listen_addr()?;
    let network_config =
        TestNetworkConfig::new(vec![listen_addr.clone()], bootnodes.to_vec(), max_peers);

    // A read-only in-memory reserve snapshot: the storer participates in
    // topology and gossip without a persistent store behind it.
    let store = Arc::new(MockStorage::default());

    let overlay = identity.overlay_address();
    let mut builder = StorerNode::builder(identity)
        .with_store(store.clone())
        .with_pullsync_storage(store);
    if let Some(override_fn) = transport_override(transport) {
        builder = builder.with_transport(override_fn);
    }
    let (mut storer, _service, _handle, _pullsync) = builder
        .build(&network_config, None)
        .await
        .wrap_err("failed to build storer node")?;
    storer
        .start_listening()
        .wrap_err("storer failed to start listening")?;

    let peer_id = *storer.local_peer_id();
    let topology = storer.topology_handle().clone();
    let listen_with_peer = listen_addr.with(libp2p::multiaddr::Protocol::P2p(peer_id));

    let join = spawn_node_task("cluster-storer", move |graceful| async move {
        storer.run(graceful).await
    });

    Ok(ClusterNodeHandle {
        role: NodeRole::Storer,
        overlay,
        peer_id,
        listen_addr: listen_with_peer,
        topology,
        join: Some(join),
    })
}

/// Spawn a node's `run` future on the executor with a graceful-shutdown signal,
/// and project its `Result<()>` out through a bridging task so we can join on a
/// typed handle. A panic in the run future is surfaced as an `eyre` error.
fn spawn_node_task<F, Fut>(name: &'static str, f: F) -> JoinHandle<Result<()>>
where
    F: FnOnce(vertex_tasks::GracefulShutdown) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    let executor = vertex_tasks::TaskExecutor::current();
    let (result_tx, result_rx) = tokio::sync::oneshot::channel::<Result<()>>();

    let spawn = executor.spawn_with_graceful_shutdown_signal(name, move |graceful| async move {
        let res = f(graceful).await;
        // The receiver is held by the bridging task below; on send failure the
        // bridge has already been dropped (test aborted), so silently discard.
        let _ = result_tx.send(res);
    });

    tokio::spawn(async move {
        // If the spawned task panicked, surface the panic message rather than
        // masking it as "task dropped without producing a result".
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
    max_peers: usize,
    peer: TestPeerConfig,
    routing: vertex_swarm_topology::KademliaConfig,
}

impl TestNetworkConfig {
    fn new(listen_addrs: Vec<Multiaddr>, bootnodes: Vec<Multiaddr>, max_peers: usize) -> Self {
        Self {
            listen_addrs,
            bootnodes,
            trusted_peers: Vec::new(),
            nat_addrs: Vec::new(),
            max_peers,
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
        self.max_peers
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
