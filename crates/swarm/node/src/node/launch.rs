//! Fluent launcher for an embedded Swarm client node.
//!
//! [`ClientLauncher`] is the lightweight entry point shared by native embedders
//! and the browser client: no database, no RPC server. It assembles a dial-only
//! [`ClientNode`] over a handful of network knobs and delegates the shared
//! client wiring (accounting, settlement, the verified chunk provider, service
//! spawning) to [`build_client_core_tail`], the same tail the native builder
//! uses, then spawns the returned run task. It hands back a [`LaunchedClient`]
//! with the handles a caller needs to observe the topology and issue chunk reads
//! and writes. Settlement is pseudosettle (the chain-free path) by default; SWAP
//! is chequebook-based and so always resolves a chain, with on-chain cashout
//! added behind `swap-chequebook`. The full native stack (persistent storage,
//! RPC, the storer reserve) still goes through `vertex-swarm-builder`.

use std::sync::Arc;
use std::time::Duration;

use eyre::Result;
use libp2p::{Multiaddr, PeerId};
use nectar_primitives::SwarmAddress;
use vertex_swarm_accounting::DefaultBandwidthConfig;
use vertex_swarm_api::{
    DefaultPeerConfig, SwarmLocalStore, SwarmNetworkConfig, SwarmNodeType, SwarmPeerConfig,
    SwarmRoutingConfig,
};
use vertex_swarm_identity::Identity;
use vertex_swarm_localstore::{ChunkStore, DEFAULT_CACHE_BUDGET_BYTES, DEFAULT_SOC_CACHE_TTL_NS};
use vertex_swarm_spec::HasSpec;
use vertex_swarm_topology::{KademliaConfig, TopologyHandle};
use vertex_tasks::TaskExecutor;

#[cfg(feature = "swap")]
use alloy_primitives::Address;

use super::client::ClientNode;
use super::core::{
    ClientNodeParts, ClientTailParams, NodeRunParts, NodeRunTaskFn, RunTaskFn, SharedAccounting,
    VerifiedChunkProvider, build_client_core_tail, single_task,
};
#[cfg(feature = "swap")]
use super::core::{ClientSwapParams, node_chain_provider};
use crate::ChunkVerifyConfig;
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

/// SWAP settlement parameters for an embedded client.
///
/// The signer and the settlement chain are not carried here: the swap service
/// signs with the launcher identity and resolves the chain from the identity's
/// spec. SWAP is chequebook-based, so enabling it requires a resolvable chain;
/// with the `swap-chequebook` feature an `rpc_url` supplies it and turns on
/// on-chain cashout of received cheques, paying out to `beneficiary`.
#[cfg(feature = "swap")]
#[derive(Clone)]
pub struct LauncherSwapConfig {
    /// Our chequebook contract address, named in the cheques we issue.
    pub chequebook: Address,
    /// The payout address received cheques may name. Defaults to the identity's
    /// Ethereum address when `None`.
    pub beneficiary: Option<Address>,
    /// Cap on the cumulative cheque value we accept from a peer before refusing
    /// further cheques.
    pub bounce_limit: u128,
    /// RPC endpoint for on-chain cashout. `None` keeps settlement chain-free
    /// (cheque exchange only, no cashout).
    #[cfg(feature = "swap-chequebook")]
    pub rpc_url: Option<String>,
}

#[cfg(feature = "swap")]
impl LauncherSwapConfig {
    /// A swap config for the given chequebook, beneficiary defaulted to the
    /// identity address. Add an `rpc_url` (under `swap-chequebook`) to supply the
    /// chain SWAP requires and turn on on-chain cashout.
    #[must_use]
    pub fn new(chequebook: Address) -> Self {
        Self {
            chequebook,
            beneficiary: None,
            bounce_limit: 0,
            #[cfg(feature = "swap-chequebook")]
            rpc_url: None,
        }
    }
}

/// Fluent launcher for an embedded Swarm client node.
///
/// This is the lightweight entry point: no database, no RPC server (a chain is
/// resolved only when SWAP is enabled). The full native stack goes through
/// `vertex-swarm-builder`.
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
    /// Verification checks applied to downloaded chunks.
    verify: ChunkVerifyConfig,
    /// Byte budget for the default in-memory cache (ignored when a store is set).
    cache_budget_bytes: u64,
    /// TTL (ns) governing single-owner-chunk freshness in the default cache.
    soc_cache_ttl_ns: u64,
    /// Caller-supplied client cache. `None` builds the default in-memory cache.
    store: Option<Arc<dyn SwarmLocalStore>>,
    /// SWAP settlement parameters. `None` keeps settlement pseudosettle-only.
    #[cfg(feature = "swap")]
    swap: Option<LauncherSwapConfig>,
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
            verify: ChunkVerifyConfig::default(),
            cache_budget_bytes: DEFAULT_CACHE_BUDGET_BYTES,
            soc_cache_ttl_ns: DEFAULT_SOC_CACHE_TTL_NS,
            store: None,
            #[cfg(feature = "swap")]
            swap: None,
        }
    }

    /// Set the verification checks applied to downloaded chunks.
    #[must_use]
    pub fn with_verify(mut self, verify: ChunkVerifyConfig) -> Self {
        self.verify = verify;
        self
    }

    /// Set the byte budget for the default in-memory cache. Ignored when a store
    /// is supplied through [`Self::with_store`].
    #[must_use]
    pub fn with_cache_budget(mut self, budget_bytes: u64) -> Self {
        self.cache_budget_bytes = budget_bytes;
        self
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
    /// Drives the pseudosettle allowance, the per-chunk price, and the admission
    /// band thresholds. Defaults to [`DefaultBandwidthConfig::default`].
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

    /// Supply the client chunk cache (served for inbound retrievals and the
    /// client's own deliveries). Defaults to an in-memory cache; a browser
    /// caller passes an IndexedDB-backed store here so the cache survives a
    /// reload.
    #[must_use]
    pub fn with_store(mut self, store: Arc<dyn SwarmLocalStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// Enable SWAP cheque settlement on top of pseudosettle.
    ///
    /// Without this the launched client settles by pseudosettle only. With the
    /// `swap-chequebook` feature and an `rpc_url` in the config, received cheques
    /// are also cashed on chain.
    #[cfg(feature = "swap")]
    #[must_use]
    pub fn with_swap(mut self, cfg: LauncherSwapConfig) -> Self {
        self.swap = Some(cfg);
        self
    }

    /// Build and start the client node, returning its handles.
    ///
    /// Assembles a [`ClientNode`] over the platform transport (TCP with DNS
    /// natively, secure websockets in the browser), wires the shared client
    /// core (pseudosettle accounting, candidate selector, the origin-gated
    /// handle, and the relay forwarder), spawns the node run loop, the client service,
    /// the pseudosettle settlement service, and the peer-manager tick on the
    /// current [`TaskExecutor`], and returns a [`LaunchedClient`]. The node
    /// dials its bootnodes as part of startup; from there the Kademlia routing
    /// table fills on its own.
    ///
    /// Pseudosettle settlement is always wired; SWAP is added when configured
    /// through `with_swap`.
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

        // The launcher owns the client cache so callers can read it back; the
        // node serves inbound retrievals and the client's own deliveries from the
        // same store. A caller-supplied store (an IndexedDB-backed cache in the
        // browser) replaces the default in-memory one.
        let store: Arc<dyn SwarmLocalStore> = self.store.unwrap_or_else(|| {
            Arc::new(ChunkStore::with_budget(
                self.cache_budget_bytes as usize,
                self.soc_cache_ttl_ns,
            ))
        });

        // SWAP is chequebook-based and so always requires the chain: resolve the
        // provider up front and hard-fail a swap-enabled client that has none.
        // Pseudosettle, wired unconditionally inside the tail, is the chain-free
        // settlement path.
        #[cfg(feature = "swap")]
        let chain_provider = {
            let swap_enabled = self.swap.is_some();
            #[cfg(feature = "swap-chequebook")]
            let rpc_url = self.swap.as_ref().and_then(|cfg| cfg.rpc_url.as_deref());
            #[cfg(not(feature = "swap-chequebook"))]
            let rpc_url: Option<&str> = None;
            node_chain_provider(
                &spec,
                &self.identity,
                SwarmNodeType::Client,
                swap_enabled,
                rpc_url,
            )
            .await
            .map_err(|e| eyre::eyre!("{e}"))?
        };

        // The launcher always builds a client, which paces against the scaled line.
        let bandwidth = self.bandwidth.for_client();

        let tail_params = ClientTailParams {
            node_type: SwarmNodeType::Client,
            spec: &spec,
            identity: &self.identity,
            bandwidth: &bandwidth,
            verify: self.verify,
            #[cfg(feature = "swap")]
            swap: ClientSwapParams {
                // An embedded client defaults SWAP off; `with_swap` turns it on.
                enable: self.swap.as_ref().map(|_| true),
                chequebook: self.swap.as_ref().map(|cfg| cfg.chequebook),
                beneficiary: self.swap.as_ref().and_then(|cfg| cfg.beneficiary),
                // The browser cannot deploy a chequebook.
                deploy: false,
                bounce_limit: self.swap.as_ref().map_or(0, |cfg| cfg.bounce_limit),
            },
        };

        let executor = TaskExecutor::current();

        // The dial-only client node is built inside the tail over the prepared
        // settlement event sinks; the launcher carries no provider store, so it
        // returns its overlay and peer id for the handles below.
        let identity = Arc::clone(&self.identity);
        let kademlia = self.kademlia;
        let store_for_node = store.clone();

        let parts: ClientNodeParts<(SwarmAddress, PeerId)> = build_client_core_tail(
            &executor,
            tail_params,
            #[cfg(feature = "swap")]
            chain_provider,
            move |events| async move {
                let node_builder = ClientNode::builder(identity)
                    .with_kademlia_config(kademlia)
                    .with_store(store_for_node)
                    .with_pseudosettle_events(events.pseudosettle);
                #[cfg(feature = "swap")]
                let node_builder = match events.swap {
                    Some(tx) => node_builder.with_swap_events(tx),
                    None => node_builder,
                };
                let (mut node, client_service, client_handle) = node_builder
                    .build(&config, None)
                    .await
                    .map_err(|e| eyre::eyre!("failed to build client node: {e}"))?;

                let topology = node.topology_handle().clone();
                let overlay = node.overlay_address();
                let peer_id = *node.local_peer_id();
                let forward_topology = topology.clone();

                // Forwarding is enabled inside the run task over the shared
                // accounting the tail builds; the node then moves into the run
                // loop. The relay legs run over the plain (ungated) handle.
                let run: RunTaskFn = Box::new(move |accounting, _reporter, client_handle| {
                    node.enable_forwarding(
                        Arc::new(forward_topology),
                        Arc::clone(&accounting),
                        client_handle,
                    );
                    single_task(move |shutdown| async move {
                        let _accounting = accounting;
                        if let Err(e) = node.start_and_run(shutdown).await {
                            tracing::error!(error = %e, "client node exited with error");
                        }
                    })
                });

                Ok::<_, eyre::Report>((
                    NodeRunParts {
                        topology,
                        client_service,
                        client_handle,
                        run,
                    },
                    (overlay, peer_id),
                ))
            },
        )
        .await?;

        let ClientNodeParts {
            task,
            topology,
            chunks,
            accounting,
            client,
            provider_store: (overlay, peer_id),
        } = parts;

        // The node run loop owns the libp2p swarm. It starts listening (a no-op
        // for a dial-only client), then dials bootnodes and services the event
        // loop for the session.
        spawn_node_run_loop(&executor, task);

        Ok(LaunchedClient {
            topology,
            client,
            accounting,
            chunks,
            store,
            overlay,
            peer_id,
        })
    }
}

/// Spawn the node run-loop task returned by the launch tail.
///
/// The native task future is `Send`, so it spawns as a critical task on the
/// tokio runtime and participates in graceful shutdown.
#[cfg(not(target_arch = "wasm32"))]
fn spawn_node_run_loop(executor: &TaskExecutor, task: NodeRunTaskFn) {
    executor.spawn_critical_with_graceful_shutdown_signal("swarm.client_node", task);
}

/// Browser variant: the websocket-transport run future is `!Send`, so the task
/// goes through the executor's local spawner instead of the Send-bounded one.
#[cfg(target_arch = "wasm32")]
fn spawn_node_run_loop(executor: &TaskExecutor, task: NodeRunTaskFn) {
    executor.spawn_local_with_graceful_shutdown_signal("swarm.client_node", task);
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
    chunks: VerifiedChunkProvider,
    store: Arc<dyn SwarmLocalStore>,
    overlay: SwarmAddress,
    peer_id: PeerId,
}

impl LaunchedClient {
    /// Topology handle for readiness polling and
    /// [`TopologyEvent`](vertex_swarm_topology::TopologyEvent) subscription.
    pub fn topology(&self) -> &TopologyHandle<Arc<Identity>> {
        &self.topology
    }

    /// Origin-gated client handle for chunk retrieval and upload.
    pub fn client(&self) -> &ClientHandle {
        &self.client
    }

    /// The selection-aware verified chunk provider: the retrieval and upload
    /// surface an embedder drives, with download verification applied.
    pub fn chunks(&self) -> &VerifiedChunkProvider {
        &self.chunks
    }

    /// The shared client accounting (selector, forwarder, service, and
    /// settlement all read this instance).
    pub fn accounting(&self) -> &SharedAccounting {
        &self.accounting
    }

    /// The client chunk cache (the default in-memory cache, or the store
    /// supplied through [`ClientLauncher::with_store`]).
    pub fn store(&self) -> &Arc<dyn SwarmLocalStore> {
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
