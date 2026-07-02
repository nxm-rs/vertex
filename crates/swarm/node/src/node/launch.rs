//! Fluent launcher for an embedded Swarm client node.
//!
//! [`ClientLauncher`] is the lightweight entry point shared by native embedders
//! and the browser client: no database, no RPC server. It carries the shared
//! domain configs ([`NetworkConfig`], [`LocalStoreConfig`], and the plain-data
//! [`SwapConfig`]) in a dial-only shape and delegates the shared client wiring
//! (accounting, settlement, the chunk provider, service spawning) to
//! [`build_client_core_tail`], the same tail the native builder uses, then spawns
//! the returned run task. It hands back a [`LaunchedClient`] with the handles a
//! caller needs to observe the topology and issue chunk reads and writes.
//! Settlement is pseudosettle (the chain-free path) by default; SWAP is
//! chequebook-based and so always resolves a chain, with on-chain cashout added
//! behind `swap-chequebook`. The full native stack (persistent storage, RPC, the
//! storer reserve) still goes through `vertex-swarm-builder`.

use std::sync::Arc;
use std::time::Duration;

use eyre::Result;
use libp2p::{Multiaddr, PeerId};
use nectar_primitives::SwarmAddress;
use vertex_swarm_accounting::DefaultAccountingConfig;
use vertex_swarm_api::{SwarmLocalStore, SwarmNodeType};
use vertex_swarm_identity::Identity;
use vertex_swarm_localstore::{ChunkStore, LocalStoreConfig};
use vertex_swarm_spec::HasSpec;
use vertex_swarm_topology::{KademliaConfig, TopologyHandle};
use vertex_tasks::TaskExecutor;

use crate::args::NetworkConfig;
#[cfg(feature = "swap")]
use crate::args::SwapConfig;

use super::client::ClientNode;
#[cfg(feature = "swap")]
use super::core::node_chain_provider;
use super::core::{
    ClientNodeParts, ClientTailParams, NativeChunkProvider, NodeRunParts, NodeRunTaskFn, RunTaskFn,
    SharedAccounting, build_client_core_tail, single_task,
};
use crate::ClientHandle;
use crate::inflight::PeerInflightLimiter;

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
    network: NetworkConfig<KademliaConfig>,
    bandwidth: DefaultAccountingConfig,
    local_store: LocalStoreConfig,
    /// Caller-supplied client cache. `None` builds the default in-memory cache.
    store: Option<Arc<dyn SwarmLocalStore>>,
    /// SWAP settlement parameters.
    #[cfg(feature = "swap")]
    swap: SwapConfig,
    /// RPC endpoint for on-chain cashout of received cheques. `None` keeps
    /// settlement chain-free (cheque exchange only, no cashout).
    #[cfg(feature = "swap-chequebook")]
    rpc_url: Option<String>,
}

impl ClientLauncher {
    /// Create a launcher for the given identity with default settings.
    #[must_use]
    pub fn new(identity: impl Into<Arc<Identity>>) -> Self {
        Self {
            identity: identity.into(),
            network: NetworkConfig::dial_only(),
            bandwidth: DefaultAccountingConfig::default(),
            local_store: LocalStoreConfig::default(),
            store: None,
            #[cfg(feature = "swap")]
            swap: SwapConfig::default(),
            #[cfg(feature = "swap-chequebook")]
            rpc_url: None,
        }
    }

    /// Set the local-store configuration for the default in-memory cache.
    /// Ignored when a store is supplied through [`Self::with_store`].
    #[must_use]
    pub fn with_local_store(mut self, config: LocalStoreConfig) -> Self {
        self.local_store = config;
        self
    }

    /// Replace the whole dial-only network configuration.
    #[must_use]
    pub fn with_network(mut self, network: NetworkConfig<KademliaConfig>) -> Self {
        self.network = network;
        self
    }

    /// Set the bootnode multiaddrs to dial at startup.
    ///
    /// When left empty, the launcher falls back to the bootnodes baked into
    /// the identity's network spec. Topology resolves those per platform: the
    /// system resolver natively, DNS-over-HTTPS in the browser.
    #[must_use]
    pub fn with_bootnodes(mut self, bootnodes: impl IntoIterator<Item = Multiaddr>) -> Self {
        self.network
            .override_bootnodes(bootnodes.into_iter().collect());
        self
    }

    /// Set the Kademlia routing configuration.
    #[must_use]
    pub fn with_kademlia(mut self, config: KademliaConfig) -> Self {
        self.network = self.network.with_routing(config);
        self
    }

    /// Set the bandwidth accounting configuration.
    ///
    /// Drives the pseudosettle allowance, the per-chunk price, and the admission
    /// band thresholds. Defaults to [`DefaultAccountingConfig::default`].
    #[must_use]
    pub fn with_bandwidth(mut self, bandwidth: DefaultAccountingConfig) -> Self {
        self.bandwidth = bandwidth;
        self
    }

    /// Set the transport-layer cap on established connections.
    #[must_use]
    pub fn with_max_peers(mut self, max: usize) -> Self {
        self.network = self.network.with_max_peers(max);
        self
    }

    /// Set the connection idle timeout.
    #[must_use]
    pub fn with_idle_timeout(mut self, timeout: Duration) -> Self {
        self.network = self.network.with_idle_timeout(timeout);
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
    /// An unset `enable` in the passed config defaults on: calling this at all
    /// means the caller wants SWAP. With the `swap-chequebook` feature and an
    /// RPC URL set through [`Self::with_swap_rpc_url`], received cheques are also
    /// cashed on chain.
    #[cfg(feature = "swap")]
    #[must_use]
    pub fn with_swap(mut self, mut cfg: SwapConfig) -> Self {
        cfg.enable.get_or_insert(true);
        self.swap = cfg;
        self
    }

    /// Set the RPC endpoint used to cash received cheques on chain.
    #[cfg(feature = "swap-chequebook")]
    #[must_use]
    pub fn with_swap_rpc_url(mut self, url: impl Into<String>) -> Self {
        self.rpc_url = Some(url.into());
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
        let config = self.network;
        // The routing config is needed by the node builder after `config` moves
        // into the build closure, so clone it out up front.
        let kademlia = config.routing().clone();

        let spec = Arc::clone(HasSpec::spec(&self.identity));

        // The launcher owns the client cache so callers can read it back; the
        // node serves inbound retrievals and the client's own deliveries from the
        // same store. A caller-supplied store (an IndexedDB-backed cache in the
        // browser) replaces the default in-memory one.
        let store: Arc<dyn SwarmLocalStore> = self.store.unwrap_or_else(|| {
            Arc::new(ChunkStore::with_budget(
                self.local_store.cache_budget_bytes() as usize,
                self.local_store.soc_cache_ttl(),
            ))
        });

        // SWAP is chequebook-based and so always requires the chain: resolve the
        // provider up front and hard-fail a swap-enabled client that has none.
        // Pseudosettle, wired unconditionally inside the tail, is the chain-free
        // settlement path.
        #[cfg(feature = "swap")]
        let chain_provider = {
            let swap_enabled = self
                .swap
                .enable
                .unwrap_or(SwarmNodeType::Client.swap_default());
            #[cfg(feature = "swap-chequebook")]
            let rpc_url = self.rpc_url.as_deref();
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
            #[cfg(feature = "swap")]
            swap: &self.swap,
        };

        let executor = TaskExecutor::current();

        // The dial-only client node is built inside the tail over the prepared
        // settlement event sinks; the launcher carries no provider store, so it
        // returns its overlay and peer id for the handles below.
        let identity = Arc::clone(&self.identity);
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

                // Forwarding is enabled inside the run task over the shared
                // accounting the tail builds and the engine's relay role; the
                // node then moves into the run loop.
                let run: RunTaskFn = Box::new(move |accounting, engine| {
                    node.enable_forwarding(engine, Arc::clone(&accounting));
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
            inflight,
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
            inflight,
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
    inflight: Arc<PeerInflightLimiter>,
    accounting: SharedAccounting,
    chunks: NativeChunkProvider,
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

    /// The per-peer retrieval in-flight limiter the launched service forgets on
    /// disconnect. An embedder driving its own retrieval engine caps against
    /// this shared instance rather than a private one.
    pub fn inflight(&self) -> &Arc<PeerInflightLimiter> {
        &self.inflight
    }

    /// The selection-aware chunk provider: the retrieval and upload surface an
    /// embedder drives.
    pub fn chunks(&self) -> &NativeChunkProvider {
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

#[cfg(all(test, feature = "swap"))]
mod tests {
    use super::*;

    fn test_launcher() -> ClientLauncher {
        let spec = vertex_swarm_spec::init_testnet();
        let identity = Arc::new(Identity::random(spec, SwarmNodeType::Client));
        ClientLauncher::new(identity)
    }

    #[test]
    fn with_swap_defaults_enable_on() {
        let launcher = test_launcher().with_swap(SwapConfig::default());
        assert_eq!(launcher.swap.enable, Some(true));
    }

    #[test]
    fn with_swap_honours_explicit_disable() {
        let cfg = SwapConfig {
            enable: Some(false),
            ..Default::default()
        };
        let launcher = test_launcher().with_swap(cfg);
        assert_eq!(launcher.swap.enable, Some(false));
    }
}
