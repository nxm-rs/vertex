//! The storer code cone, gated behind the `reserve` feature.
//!
//! Concentrates everything the storer node type adds over the default client: the
//! persisting reserve, the neighbourhood puller, the pullsync and redistribution
//! wiring, the cache-then-reserve serve view, and the storer config, builder, and
//! launch path. The shared launch and builder code stays capability-agnostic and
//! the storer plugs into it through the [`NodeAssembly`] seam and the
//! [`StorerNodeBuilder`] wrapper around the client builder.

mod composite;
mod pullsync;

use std::sync::Arc;

use tracing::warn;

use vertex_node_api::{InfrastructureContext, NodeBuildsProtocol};
use vertex_storage_redb::RedbDatabase;
use vertex_swarm_accounting::DefaultBandwidthConfig;
use vertex_swarm_api::{
    BinCursorStore, PeerReporter, PullChunkVerifier, PullStorage, ReserveStore, StorageRadius,
    StorerComponents, SwarmAccountingConfig, SwarmIdentity, SwarmLaunchConfig, SwarmLocalStore,
    SwarmLocalStoreConfig, SwarmNetworkConfig, SwarmNodeType, SwarmPeerConfig, SwarmPricingConfig,
    SwarmRoutingConfig, SwarmStorageConfig, construct,
};
use vertex_swarm_identity::Identity;
use vertex_swarm_localstore::LocalStoreConfig;
use vertex_swarm_node::args::{ChainConfig, NetworkConfig, SwapConfig};
use vertex_swarm_node::{StorerNode, StorerPullsyncControl};
use vertex_swarm_postage::{AdmissionValidator, DbBatchStore};
use vertex_swarm_puller::{
    FundingVerifier, PullerConfig, PullerHandle, PullerSeams, SignatureVerifier, spawn_puller,
};
use vertex_swarm_redistribution::StorageConfig;
use vertex_swarm_spec::Spec;
use vertex_swarm_storer::{DbIntervalStore, DbReserve, EvictionStrategy};
use vertex_swarm_topology::{KademliaConfig, TopologyHandle};
use vertex_tasks::NodeTaskFn;

use crate::error::SwarmNodeError;
use crate::handle::{BuiltNode, BuiltStorer};
use crate::launch::{
    AssemblyInputs, CacheSeam, ClientLaunchTypes, ClientNodeParams, NodeAssembly,
    build_client_backed_node, resolve_cache,
};
use crate::node::{ClientNodeBuilder, NodeBuilder};
use crate::protocol::SwarmProtocol;
use vertex_swarm_node::{NativeChunkProvider, NodeRunParts, RunTaskFn, single_task};

/// A reserve override supplied through the builder. With no seam the storer launch
/// path builds the default admission-gated [`DbReserve`] over the shared database.
pub(crate) enum ReserveSeam {
    /// A pre-built reserve, used as-is.
    Ready(Arc<dyn BinCursorStore>),
    /// A factory invoked at build time with the opened shared database.
    Factory(ReserveFactory),
}

/// Builds a reserve from the opened shared database (if any).
pub(crate) type ReserveFactory = Box<
    dyn FnOnce(Option<Arc<RedbDatabase>>) -> Result<Arc<dyn BinCursorStore>, SwarmNodeError> + Send,
>;

/// Validated configuration for storer (full) node with storage and redistribution.
#[derive(Clone)]
pub struct StorerConfig {
    spec: Arc<Spec>,
    identity: Arc<Identity>,
    network: NetworkConfig<KademliaConfig>,
    bandwidth: DefaultBandwidthConfig,
    local_store: LocalStoreConfig,
    storage: StorageConfig,
    chain: ChainConfig,
    swap: SwapConfig,
}

impl StorerConfig {
    #[expect(
        clippy::too_many_arguments,
        reason = "a storer aggregates every validated config section"
    )]
    pub fn new(
        spec: Arc<Spec>,
        identity: Arc<Identity>,
        network: NetworkConfig<KademliaConfig>,
        bandwidth: DefaultBandwidthConfig,
        local_store: LocalStoreConfig,
        storage: StorageConfig,
        chain: ChainConfig,
        swap: SwapConfig,
    ) -> Self {
        Self {
            spec,
            identity,
            network,
            bandwidth,
            local_store,
            storage,
            chain,
            swap,
        }
    }

    pub fn spec(&self) -> &Arc<Spec> {
        &self.spec
    }

    pub fn identity(&self) -> &Arc<Identity> {
        &self.identity
    }

    pub fn network(&self) -> &NetworkConfig<KademliaConfig> {
        &self.network
    }

    pub fn bandwidth(&self) -> &DefaultBandwidthConfig {
        &self.bandwidth
    }

    pub fn local_store(&self) -> &LocalStoreConfig {
        &self.local_store
    }

    pub fn storage(&self) -> &StorageConfig {
        &self.storage
    }

    pub fn chain(&self) -> &ChainConfig {
        &self.chain
    }

    /// SWAP settlement configuration (chequebook, beneficiary, deploy).
    pub fn swap(&self) -> &SwapConfig {
        &self.swap
    }
}

impl NodeBuildsProtocol for StorerConfig {
    type Protocol = SwarmProtocol<Self>;

    fn protocol_name(&self) -> &'static str {
        "Swarm Storer"
    }
}

/// Builder for storer nodes. Wraps the client builder, carrying the storage and
/// reserve seams the storer build path consumes.
pub struct StorerNodeBuilder<I, N, A, S, St>
where
    I: SwarmIdentity,
    N: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig,
    A: SwarmAccountingConfig + SwarmPricingConfig,
    S: SwarmLocalStoreConfig,
    St: SwarmStorageConfig,
{
    client: ClientNodeBuilder<I, N, A>,
    local_store: S,
    storage: St,
    /// `None` builds the default admission-gated reserve over the shared database.
    reserve: Option<ReserveSeam>,
}

impl<I, N, A, S, St> StorerNodeBuilder<I, N, A, S, St>
where
    I: SwarmIdentity,
    N: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig,
    A: SwarmAccountingConfig + SwarmPricingConfig,
    S: SwarmLocalStoreConfig,
    St: SwarmStorageConfig,
{
    /// Wrap a client builder with the storer storage and reserve seams.
    pub(crate) fn from_client(
        client: ClientNodeBuilder<I, N, A>,
        local_store: S,
        storage: St,
    ) -> Self {
        Self {
            client,
            local_store,
            storage,
            reserve: None,
        }
    }

    pub fn spec(&self) -> &Arc<Spec> {
        self.client.spec()
    }

    /// Override the cache with a pre-built local store. See
    /// [`ClientNodeBuilder::with_cache`].
    pub fn with_cache(mut self, cache: Arc<dyn SwarmLocalStore>) -> Self {
        self.client = self.client.with_cache(cache);
        self
    }

    /// Override the cache with a factory. See
    /// [`ClientNodeBuilder::with_cache_factory`].
    pub fn with_cache_factory<F>(mut self, factory: F) -> Self
    where
        F: FnOnce(Option<Arc<RedbDatabase>>) -> Result<Arc<dyn SwarmLocalStore>, SwarmNodeError>
            + Send
            + 'static,
    {
        self.client = self.client.with_cache_factory(factory);
        self
    }

    /// Override the storer reserve with a pre-built store.
    ///
    /// The reserve must implement [`BinCursorStore`] so the served reserve
    /// capabilities can query per-bin counts and insertion cursors.
    pub fn with_reserve(mut self, reserve: Arc<dyn BinCursorStore>) -> Self {
        self.reserve = Some(ReserveSeam::Ready(reserve));
        self
    }

    /// Override the storer reserve with a factory invoked at build time.
    ///
    /// The factory receives the opened shared database (`None` in-memory). The
    /// reserve must implement [`BinCursorStore`].
    pub fn with_reserve_factory<F>(mut self, factory: F) -> Self
    where
        F: FnOnce(Option<Arc<RedbDatabase>>) -> Result<Arc<dyn BinCursorStore>, SwarmNodeError>
            + Send
            + 'static,
    {
        self.reserve = Some(ReserveSeam::Factory(Box::new(factory)));
        self
    }
}

/// Default storer builder.
pub type DefaultStorerBuilder = StorerNodeBuilder<
    Arc<Identity>,
    NetworkConfig<KademliaConfig>,
    DefaultBandwidthConfig,
    LocalStoreConfig,
    StorageConfig,
>;

impl DefaultStorerBuilder {
    pub fn from_parts(
        spec: Arc<Spec>,
        identity: Arc<Identity>,
        network: NetworkConfig<KademliaConfig>,
        bandwidth: DefaultBandwidthConfig,
        local_store: LocalStoreConfig,
        storage: StorageConfig,
    ) -> Self {
        let client = NodeBuilder::new(spec, identity, network).with_accounting(bandwidth);
        Self::from_client(client, local_store, storage)
    }

    pub fn from_config(config: StorerConfig) -> Self {
        let chain = config.chain().clone();
        let swap = config.swap().clone();
        let mut builder = Self::from_parts(
            config.spec().clone(),
            config.identity().clone(),
            config.network().clone(),
            config.bandwidth().clone(),
            config.local_store().clone(),
            config.storage().clone(),
        );
        builder.client = builder.client.with_chain(chain).with_swap(swap);
        builder
    }

    /// Convert to config for building.
    pub fn into_config(self) -> StorerConfig {
        StorerConfig::new(
            self.client.base.spec,
            self.client.base.identity,
            self.client.base.network,
            self.client.accounting,
            self.local_store,
            self.storage,
            self.client.chain,
            self.client.swap,
        )
    }

    /// Build the storer node, honoring any cache or reserve seam set on the
    /// builder.
    pub async fn build(
        mut self,
        ctx: &dyn InfrastructureContext,
    ) -> Result<BuiltStorer, SwarmNodeError> {
        let cache = self.client.cache.take();
        let reserve = self.reserve.take();
        let config = self.into_config();
        let (task, providers) = build_storer(config, ctx, cache, reserve).await?;
        Ok(BuiltNode::new(task, providers))
    }
}

impl From<StorerConfig> for DefaultStorerBuilder {
    fn from(config: StorerConfig) -> Self {
        Self::from_config(config)
    }
}

impl SwarmLaunchConfig for StorerConfig {
    type Types = ClientLaunchTypes;
    type Providers = StorerProviders;
    type Error = SwarmNodeError;

    async fn build(
        self,
        ctx: &dyn InfrastructureContext,
    ) -> Result<(NodeTaskFn, Self::Providers), Self::Error> {
        build_storer(self, ctx, None, None).await
    }
}

/// Shared storer RPC provider bundle: topology, the chunk provider, the serve
/// view, and the reserve.
type StorerProviders = StorerComponents<
    TopologyHandle<Arc<Identity>>,
    NativeChunkProvider,
    Arc<dyn SwarmLocalStore>,
    Arc<dyn BinCursorStore>,
>;

/// Build a storer node, optionally overriding the cache and reserve through
/// builder seams.
///
/// Both `None` reproduces the default: the admission-gated [`DbReserve`] is the
/// pushsync-ingest reserve, layered under a default in-memory forwarding cache for
/// the retrieval-serve view. A reserve seam replaces the reserve; a cache seam
/// replaces the forwarding cache.
pub(crate) async fn build_storer(
    config: StorerConfig,
    ctx: &dyn InfrastructureContext,
    cache: Option<CacheSeam>,
    reserve: Option<ReserveSeam>,
) -> Result<(NodeTaskFn, StorerProviders), SwarmNodeError> {
    // Reserve capacity is a consensus quantity read from the spec, not local disk:
    // a fixed power-of-two chunk count from which the redistribution game derives
    // storage radius and committed depth, so nodes covering one neighbourhood must
    // agree on it regardless of disk.
    let _redistribution_enabled = config.storage().redistribution_enabled();
    let capacity = config.spec().reserve_capacity;
    let identity = config.identity().clone();
    let cache_budget = config.local_store().cache_budget_bytes();
    let soc_ttl = config.local_store().soc_cache_ttl();

    let parts = build_client_backed_node(
        ctx,
        ClientNodeParams {
            spec: config.spec(),
            identity: config.identity(),
            network: config.network(),
            bandwidth: config.bandwidth(),
            #[cfg(feature = "swap")]
            chain: config.chain(),
            #[cfg(feature = "swap")]
            swap: config.swap(),
        },
        StorerAssembly::new(cache, reserve, identity, capacity, cache_budget, soc_ttl),
    )
    .await?;

    let (store, reserve) = parts.provider_store;
    let providers = construct::storer(parts.topology, parts.chunks, store, reserve);
    Ok((parts.task, providers))
}

/// Guard message: a storer whose reserve is not the default `DbReserve` cannot
/// serve pullsync.
const STORER_PULLSYNC_MISSING: &str = "storer pullsync reserve view missing";

/// Block confirmations a batch must accrue before the reserve admits chunks
/// stamped under it, so a reorg cannot retroactively invalidate admitted chunks.
const RESERVE_CONFIRMATION_THRESHOLD: u64 = 10;

/// The storer assembly: builds the persisting reserve, layers the serve view over
/// it, and assembles the pullsync-capable [`StorerNode`] plus its puller.
struct StorerAssembly {
    cache: Option<CacheSeam>,
    reserve_seam: Option<ReserveSeam>,
    identity: Arc<Identity>,
    capacity: u64,
    cache_budget_bytes: u64,
    soc_cache_ttl: u64,
}

impl StorerAssembly {
    fn new(
        cache: Option<CacheSeam>,
        reserve_seam: Option<ReserveSeam>,
        identity: Arc<Identity>,
        capacity: u64,
        cache_budget_bytes: u64,
        soc_cache_ttl: u64,
    ) -> Self {
        Self {
            cache,
            reserve_seam,
            identity,
            capacity,
            cache_budget_bytes,
            soc_cache_ttl,
        }
    }
}

#[async_trait::async_trait]
impl NodeAssembly for StorerAssembly {
    const NODE_TYPE: SwarmNodeType = SwarmNodeType::Storer;

    type ProviderStore = (Arc<dyn SwarmLocalStore>, Arc<dyn BinCursorStore>);

    async fn assemble(
        self,
        ctx: &dyn InfrastructureContext,
        inputs: AssemblyInputs<'_>,
    ) -> Result<(NodeRunParts, Self::ProviderStore), SwarmNodeError> {
        let serve = build_serve_store(
            self.reserve_seam,
            self.cache,
            inputs.db.clone(),
            &self.identity,
            self.capacity,
            self.cache_budget_bytes,
            self.soc_cache_ttl,
        )?;
        let provider_store = (Arc::clone(&serve.local), Arc::clone(&serve.reserve));
        let capable = assemble_storer_node(
            ctx,
            inputs.identity,
            inputs.network,
            serve.local,
            inputs.peer_store,
            inputs.db,
            serve.reserve,
            serve.pullsync,
            serve.batches,
            inputs.pseudosettle_event_sender,
            #[cfg(feature = "swap")]
            inputs.swap_event_sender,
        )
        .await?;
        Ok((capable, provider_store))
    }
}

/// The storer serve store: the `CacheThenReserve` view the node reads and writes
/// through, plus the reserve and its optional pullsync/batch views (the latter two
/// present only for the default [`DbReserve`]).
struct StorerServeStore {
    local: Arc<dyn SwarmLocalStore>,
    reserve: Arc<dyn BinCursorStore>,
    pullsync: Option<Arc<dyn PullStorage>>,
    batches: Option<DbBatchStore<RedbDatabase>>,
}

/// Build the reserve (or the seam override) and layer the forwarding cache over it.
///
/// The pullsync server snapshot and batch handle exist only for the default
/// `DbReserve`; a reserve seam erases to `BinCursorStore`, leaving pullsync inbound
/// serving unwired for that override.
fn build_serve_store(
    reserve_seam: Option<ReserveSeam>,
    cache: Option<CacheSeam>,
    db: Option<Arc<RedbDatabase>>,
    identity: &Arc<Identity>,
    capacity: u64,
    cache_budget_bytes: u64,
    soc_cache_ttl: u64,
) -> Result<StorerServeStore, SwarmNodeError> {
    let (reserve, pullsync, batches) = match reserve_seam {
        None => {
            let built = build_storer_reserve(db.clone(), identity, capacity)?;
            (built.reserve, built.pullsync, built.batches)
        }
        Some(ReserveSeam::Ready(reserve)) => (reserve, None, None),
        Some(ReserveSeam::Factory(factory)) => (factory(db.clone())?, None, None),
    };
    let cache = resolve_cache(cache, db, cache_budget_bytes, soc_cache_ttl)?;
    // The reserve upcasts to the local-store read side; writes land in the cache.
    let local: Arc<dyn SwarmLocalStore> = Arc::new(composite::CacheThenReserve::new(
        cache,
        Arc::clone(&reserve) as Arc<dyn SwarmLocalStore>,
    ));
    Ok(StorerServeStore {
        local,
        reserve,
        pullsync,
        batches,
    })
}

/// Assemble a `StorerNode` with the reserve-backed pullsync syncer, spawn its
/// puller over the topology seams, and return the run-task factory.
#[allow(clippy::too_many_arguments)]
async fn assemble_storer_node(
    ctx: &dyn InfrastructureContext,
    identity: &Arc<Identity>,
    network: &NetworkConfig<KademliaConfig>,
    node_store: Arc<dyn SwarmLocalStore>,
    peer_store: Option<crate::launch::PeerStore>,
    db: Option<Arc<RedbDatabase>>,
    reserve: Arc<dyn BinCursorStore>,
    pullsync: Option<Arc<dyn PullStorage>>,
    batches: Option<DbBatchStore<RedbDatabase>>,
    pseudosettle_event_sender: tokio::sync::mpsc::UnboundedSender<
        vertex_swarm_node::PseudosettleEvent,
    >,
    #[cfg(feature = "swap")] swap_event_sender: Option<
        tokio::sync::mpsc::UnboundedSender<vertex_swarm_node::SwapEvent>,
    >,
) -> Result<NodeRunParts, SwarmNodeError> {
    // A storer without a `PullStorage`-shaped reserve cannot serve pullsync; this
    // only happens with a reserve seam that is not the default `DbReserve`.
    let pullsync_storage =
        pullsync.ok_or_else(|| SwarmNodeError::Build(STORER_PULLSYNC_MISSING.into()))?;

    let node_builder = StorerNode::builder(identity.clone())
        .with_store(node_store)
        .with_pullsync_storage(pullsync_storage)
        .with_pseudosettle_events(pseudosettle_event_sender);
    #[cfg(feature = "swap")]
    let node_builder = match swap_event_sender {
        Some(tx) => node_builder.with_swap_events(tx),
        None => node_builder,
    };
    let (mut node, client_service, client_handle, pullsync_control) = node_builder
        .build(network, peer_store)
        .await
        .map_err(|e| SwarmNodeError::Build(e.into()))?;
    let topology = node.topology_handle().clone();

    // The reserve is the puller's admit seam (a `SwarmLocalStore` put) and the
    // pushsync ingest store; the interval store persists per-peer sync progress.
    let intervals = open_interval_store(db)?;
    // The puller consumes the node's pullsync control (its commands reach the run
    // loop and dispatch to the pullsync sub-behaviour); the node forwards delivered
    // pullsync events back through the returned handle.
    let puller_handle = spawn_storer_puller(
        ctx,
        topology.clone(),
        reserve.clone(),
        intervals,
        pullsync_control,
        batches,
    );
    node.set_puller(puller_handle);
    let forward_topology = topology.clone();

    let run: RunTaskFn = Box::new(move |accounting, _reporter, client_handle| {
        node.enable_forwarding(
            Arc::new(forward_topology),
            Arc::clone(&accounting),
            client_handle,
        );
        node.enable_storage(reserve as Arc<dyn ReserveStore>);
        single_task(move |shutdown| async move {
            let _accounting = accounting;
            if let Err(e) = node.start_and_run(shutdown).await {
                tracing::error!(error = %e, "Storer node error");
            }
        })
    });

    Ok(NodeRunParts {
        topology,
        client_service,
        client_handle,
        run,
    })
}

/// Open the puller's interval store over the shared database, or an in-memory
/// database when persistence is off (intervals reset on restart, matching the
/// in-memory reserve).
fn open_interval_store(
    db: Option<Arc<RedbDatabase>>,
) -> Result<Arc<DbIntervalStore<RedbDatabase>>, SwarmNodeError> {
    let db = match db {
        Some(db) => db,
        None => RedbDatabase::in_memory()
            .map_err(|e| SwarmNodeError::Build(e.into()))?
            .into_arc(),
    };
    DbIntervalStore::new(db)
        .map(Arc::new)
        .map_err(|e| SwarmNodeError::Build(e.into()))
}

/// Spawn the neighbourhood puller, returning the handle the node forwards pullsync
/// events through. The control surface lives on the node side and is driven by the
/// puller's `PullsyncControl` command channel.
fn spawn_storer_puller(
    ctx: &dyn InfrastructureContext,
    topology: TopologyHandle<Arc<Identity>>,
    reserve: Arc<dyn BinCursorStore>,
    intervals: Arc<DbIntervalStore<RedbDatabase>>,
    control: StorerPullsyncControl,
    batches: Option<DbBatchStore<RedbDatabase>>,
) -> PullerHandle {
    // With the default reserve the puller verifies on-chain batch funding against
    // the same batch set the reserve admits against; a reserve seam leaves no batch
    // handle, so the puller falls back to the signature-only gate.
    let verifier: Box<dyn PullChunkVerifier> = match batches {
        Some(batches) => Box::new(FundingVerifier::new(
            batches,
            AdmissionValidator::new(RESERVE_CONFIRMATION_THRESHOLD),
        )),
        None => Box::new(SignatureVerifier),
    };

    // The peer manager is the reporting authority: a neighbour that serves an
    // unverifiable chunk is scored down through it, the same path accounting and
    // the protocol handlers use.
    let reporter: Arc<dyn PeerReporter> = topology.peer_manager().clone();

    let seams = PullerSeams {
        control,
        intervals,
        verifier,
        // The reserve admits through its `SwarmLocalStore` put (the blanket
        // `ReserveAdmit` impl).
        admit: reserve as Arc<dyn SwarmLocalStore>,
        readiness: pullsync::TopologyReadiness::new(topology.clone()),
        neighbours: pullsync::TopologyNeighbours::new(topology),
        reporter,
    };
    spawn_puller(ctx.executor(), seams, PullerConfig::default())
}

/// The storer reserve and its derived views, built from the shared database.
struct BuiltReserve {
    /// The reserve as the local-store read side and pushsync ingest view.
    reserve: Arc<dyn BinCursorStore>,
    /// The reserve as the pullsync server snapshot.
    pullsync: Option<Arc<dyn PullStorage>>,
    /// A second handle onto the reserve's batch set for the puller's verifier.
    batches: Option<DbBatchStore<RedbDatabase>>,
}

/// Build the storer reserve over the shared database.
///
/// Reuses the opened database when present so the reserve, its batch store and the
/// peer store share one handle; falls back to in-memory redb without persistence.
/// Open and table-creation failures surface as a build error.
///
/// Admits only stamped chunks, gated by a `DbBatchStore` (the batch set) and an
/// `AdmissionValidator` enforcing [`RESERVE_CONFIRMATION_THRESHOLD`] confirmations
/// plus structural and signature checks. The batch store starts empty, so the
/// reserve admits nothing until the postage indexer populates it.
fn build_storer_reserve(
    db: Option<Arc<RedbDatabase>>,
    identity: &Arc<Identity>,
    capacity: u64,
) -> Result<BuiltReserve, SwarmNodeError> {
    let db = match db {
        Some(db) => db,
        None => {
            warn!("Storer reserve running in-memory; stored chunks are lost on shutdown");
            RedbDatabase::in_memory()
                .map_err(|e| SwarmNodeError::Build(e.into()))?
                .into_arc()
        }
    };
    // Batch store and reserve share one handle so the postage ingest seam and the
    // reserve see a single consistent view. The clone is a second handle onto the
    // same tables, carried out for the puller's funding verifier.
    let batches =
        DbBatchStore::new(Arc::clone(&db)).map_err(|e| SwarmNodeError::Build(e.into()))?;
    let admission = AdmissionValidator::new(RESERVE_CONFIRMATION_THRESHOLD);
    let reserve = Arc::new(
        DbReserve::new(
            db,
            identity.as_ref(),
            batches.clone(),
            admission,
            capacity,
            EvictionStrategy::EvictFurthest,
            StorageRadius::ZERO,
        )
        .map_err(|e| SwarmNodeError::Build(e.into()))?,
    );
    // One `DbReserve`, three trait-object views: local-store (node and components),
    // reserve (pushsync ingest plus the served reserve capabilities), and pullsync
    // server snapshot (the inbound syncer's cursor and range source).
    Ok(BuiltReserve {
        reserve: Arc::clone(&reserve) as Arc<dyn BinCursorStore>,
        pullsync: Some(Arc::clone(&reserve) as Arc<dyn PullStorage>),
        batches: Some(batches),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use vertex_swarm_test_utils::test_identity_arc;

    /// Test SOC cache TTL: any non-zero value works for the cache-shape tests.
    const DEFAULT_SOC_CACHE_TTL_NS_TEST: u64 = vertex_swarm_localstore::DEFAULT_SOC_CACHE_TTL_NS;

    /// The storer reserve is the admission-gated reserve, not the cache-only client
    /// store: a put for an unknown batch is rejected, proving admission is wired.
    /// The full admissible-put path is covered by the reserve crate.
    #[test]
    fn storer_reserve_builds_admission_gated_store() {
        use alloy_primitives::{B256, Signature};
        use nectar_postage::Stamp;
        use nectar_primitives::{AnyChunk, ContentChunk};
        use vertex_swarm_primitives::CachedChunk;

        let identity = test_identity_arc();
        // A small power of two matches the consensus chunk-count shape cheaply.
        let capacity: u64 = 1 << 12;

        // db = None exercises the in-memory fallback.
        let built = build_storer_reserve(None, &identity, capacity).expect("reserve builds");
        let store: Arc<dyn SwarmLocalStore> =
            Arc::clone(&built.reserve) as Arc<dyn SwarmLocalStore>;

        // A fresh storer reserve knows no batches, so serving a chunk it never held
        // returns nothing rather than erroring.
        let absent_chunk: AnyChunk = ContentChunk::new(b"never stored".to_vec())
            .expect("valid content chunk")
            .into();
        let absent = *absent_chunk.address();
        assert!(
            store
                .get(&absent)
                .expect("reserve get does not error")
                .is_none(),
            "an empty reserve serves nothing"
        );

        // A stamped put for a batch unknown to the empty batch store is rejected by
        // the wired admission validator, proving the reserve did not fall back to a
        // store that accepts arbitrary chunks.
        let chunk: AnyChunk = ContentChunk::new(b"unadmissible".to_vec())
            .expect("valid content chunk")
            .into();
        let sig = Signature::from_raw(&[1u8; 65]).expect("valid signature");
        let stamp = Stamp::new(B256::repeat_byte(0xaa), 3, 7, 42, sig);
        let cached = CachedChunk::new(chunk, Some(stamp));
        let address = *cached.address();

        let err = store
            .put(cached)
            .expect_err("a stamp for an unknown batch must be refused by admission");
        assert!(
            matches!(err, vertex_swarm_api::SwarmError::InvalidChunk { .. }),
            "admission rejects the put as an invalid chunk, got {err:?}"
        );
        assert!(
            !store.contains(&address),
            "a rejected put leaves nothing in the reserve"
        );
    }

    /// A ready reserve seam reaches the node store as the ingest view, and the
    /// serve view writes land in the cache only.
    #[test]
    fn storer_assembly_honors_a_ready_reserve_seam() {
        use nectar_primitives::{AnyChunk, ContentChunk};
        use vertex_swarm_primitives::CachedChunk;

        let identity = test_identity_arc();
        let seam_reserve = build_storer_reserve(None, &identity, 1 << 12)
            .expect("reserve builds")
            .reserve;

        let serve = build_serve_store(
            Some(ReserveSeam::Ready(Arc::clone(&seam_reserve))),
            None,
            None,
            &identity,
            1 << 12,
            1 << 20,
            DEFAULT_SOC_CACHE_TTL_NS_TEST,
        )
        .expect("seam reserve is used");
        assert!(
            Arc::ptr_eq(&seam_reserve, &serve.reserve),
            "the supplied reserve must reach the node store as the ingest view"
        );

        // A put through the serve view lands in the cache, never the reserve.
        let chunk: AnyChunk = ContentChunk::new(b"forwarded out-of-aor".to_vec())
            .expect("valid content chunk")
            .into();
        let address = *chunk.address();
        serve
            .local
            .put(CachedChunk::new(chunk, None))
            .expect("the forwarding cache accepts a content chunk");
        assert!(
            serve.local.contains(&address),
            "the retrieval-serve view serves the cached chunk"
        );
        assert!(
            !serve.reserve.contains(&address),
            "a put through the serve view must not reach the reserve"
        );
    }

    /// The default storer path (no seams) layers the default cache over the built
    /// reserve, with serve-view writes reaching the cache only.
    #[test]
    fn storer_assembly_default_layers_cache_over_built_reserve() {
        use nectar_primitives::{AnyChunk, ContentChunk};
        use vertex_swarm_primitives::CachedChunk;

        let identity = test_identity_arc();
        let serve = build_serve_store(
            None,
            None,
            None,
            &identity,
            1 << 12,
            1 << 20,
            DEFAULT_SOC_CACHE_TTL_NS_TEST,
        )
        .expect("default storer store builds");

        // A put through the serve view lands in the default cache, never the reserve.
        let chunk: AnyChunk = ContentChunk::new(b"forwarded out-of-aor default".to_vec())
            .expect("valid content chunk")
            .into();
        let address = *chunk.address();
        serve
            .local
            .put(CachedChunk::new(chunk, None))
            .expect("the default forwarding cache accepts a content chunk");
        assert!(
            serve.local.contains(&address),
            "the retrieval-serve view serves the cached chunk"
        );
        assert!(
            !serve.reserve.contains(&address),
            "a put through the serve view must not reach the built reserve"
        );
    }
}
