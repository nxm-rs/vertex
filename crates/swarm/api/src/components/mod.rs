//! Component containers and access traits for Swarm nodes.

mod bandwidth;
mod localstore;
mod peers;
mod pricing;
mod pullsync;
mod reserve;
mod topology;

pub use self::bandwidth::{
    BandwidthDebit, BandwidthReserve, Commit, CommitOnWrite, Direction, HeldReceive,
    SwarmAccountingConfig, SwarmBandwidthAccounting, SwarmClientAccounting, SwarmPeerBandwidth,
    SwarmPeerState, SwarmSettlementProvider,
};
pub use self::localstore::{SwarmLocalStore, SwarmLocalStoreConfig};
pub use self::peers::SwarmPeerResolver;
pub use self::pricing::{SwarmPricing, SwarmPricingBuilder, SwarmPricingConfig};
pub use self::pullsync::{IntervalStore, PullChunkVerifier, PullStorage, VerifyError};
pub use self::reserve::{BinCursorStore, BinScanItem, ReserveStore, SettableRadius};
pub use self::topology::{
    SwarmTopology, SwarmTopologyBins, SwarmTopologyCommands, SwarmTopologyPeers,
    SwarmTopologyReporting, SwarmTopologyRouting, SwarmTopologyState, SwarmTopologyStats,
};

use crate::SwarmIdentity;

/// Topology access.
#[auto_impl::auto_impl(&, Arc, Box)]
pub trait HasTopology: Send + Sync {
    /// The topology type.
    type Topology: Send + Sync;

    /// Get the topology.
    fn topology(&self) -> &Self::Topology;
}

/// Chunk client access (client/storer levels), driving uploads and downloads.
#[auto_impl::auto_impl(&, Arc, Box)]
pub trait HasChunkClient: Send + Sync {
    /// The chunk client type.
    type ChunkClient: Send + Sync;

    /// Get the chunk client.
    fn chunk_client(&self) -> &Self::ChunkClient;
}

/// Identity access.
pub trait HasIdentity: Send + Sync {
    /// The identity type.
    type Identity: SwarmIdentity;

    /// Get the identity.
    fn identity(&self) -> &Self::Identity;
}

/// Local store access (storer level).
#[auto_impl::auto_impl(&, Arc, Box)]
pub trait HasStore: Send + Sync {
    /// The store type.
    type Store: Send + Sync;

    /// Get the local store.
    fn store(&self) -> &Self::Store;
}

/// Reserve access (storer level).
///
/// Narrows [`HasStore`] to the proximity-ordered, always-stamped reserve so the
/// redistribution and sync subsystems can query radius, capacity, and per-bin
/// insertion order without naming the concrete store type.
#[auto_impl::auto_impl(&, Arc, Box)]
pub trait HasReserve: HasStore {
    /// The reserve type.
    type Reserve: BinCursorStore;

    /// Get the reserve.
    fn reserve(&self) -> &Self::Reserve;
}

/// Bootnode components (topology only). Identity via `topology().identity()`.
///
/// Construction is builder-exclusive; see [`construct`].
#[derive(Debug, Clone)]
pub struct BootnodeComponents<T> {
    topology: T,
}

impl<T> BootnodeComponents<T> {
    pub(crate) fn new(topology: T) -> Self {
        Self { topology }
    }
}

impl<T: Send + Sync> HasTopology for BootnodeComponents<T> {
    type Topology = T;

    fn topology(&self) -> &T {
        &self.topology
    }
}

/// Client components (topology + chunk client).
///
/// Accounting is intentionally absent: it is a builder-wired internal of the
/// network chunk client, not a served capability.
///
/// Construction is builder-exclusive; see [`construct`].
#[derive(Debug, Clone)]
pub struct ClientComponents<T, C> {
    topology: T,
    chunk_client: C,
}

impl<T, C> ClientComponents<T, C> {
    pub(crate) fn new(topology: T, chunk_client: C) -> Self {
        Self {
            topology,
            chunk_client,
        }
    }
}

impl<T: Send + Sync, C: Send + Sync> HasTopology for ClientComponents<T, C> {
    type Topology = T;

    fn topology(&self) -> &T {
        &self.topology
    }
}

impl<T: Send + Sync, C: Send + Sync> HasChunkClient for ClientComponents<T, C> {
    type ChunkClient = C;

    fn chunk_client(&self) -> &C {
        &self.chunk_client
    }
}

/// Storer components (client + local store + reserve).
///
/// `S` is the retrieval-serve view ([`HasStore`]); `R` is the proximity-ordered
/// reserve ([`HasReserve`]). Both stay generic so this crate names no concrete
/// backend. Construction is builder-exclusive; see [`construct`].
#[derive(Debug, Clone)]
pub struct StorerComponents<T, C, S, R> {
    client: ClientComponents<T, C>,
    store: S,
    reserve: R,
}

impl<T, C, S, R> StorerComponents<T, C, S, R> {
    pub(crate) fn new(topology: T, chunk_client: C, store: S, reserve: R) -> Self {
        Self {
            client: ClientComponents::new(topology, chunk_client),
            store,
            reserve,
        }
    }
}

impl<T: Send + Sync, C: Send + Sync, S: Send + Sync, R: Send + Sync> HasTopology
    for StorerComponents<T, C, S, R>
{
    type Topology = T;

    fn topology(&self) -> &T {
        self.client.topology()
    }
}

impl<T: Send + Sync, C: Send + Sync, S: Send + Sync, R: Send + Sync> HasChunkClient
    for StorerComponents<T, C, S, R>
{
    type ChunkClient = C;

    fn chunk_client(&self) -> &C {
        self.client.chunk_client()
    }
}

impl<T: Send + Sync, C: Send + Sync, S: Send + Sync, R: Send + Sync> HasStore
    for StorerComponents<T, C, S, R>
{
    type Store = S;

    fn store(&self) -> &S {
        &self.store
    }
}

impl<T: Send + Sync, C: Send + Sync, S: Send + Sync, R: BinCursorStore> HasReserve
    for StorerComponents<T, C, S, R>
{
    type Reserve = R;

    fn reserve(&self) -> &R {
        &self.reserve
    }
}

/// Builder-only construction seam for the component containers.
///
/// Containers wire shared `Arc`s that only the builder assembles correctly, so
/// they expose no public constructors; the builder and in-process node reach
/// construction through these `#[doc(hidden)]` functions. Not part of the stable
/// API.
#[doc(hidden)]
pub mod construct {
    use super::{BootnodeComponents, ClientComponents, StorerComponents};

    pub fn bootnode<T>(topology: T) -> BootnodeComponents<T> {
        BootnodeComponents::new(topology)
    }

    pub fn client<T, C>(topology: T, chunk_client: C) -> ClientComponents<T, C> {
        ClientComponents::new(topology, chunk_client)
    }

    pub fn storer<T, C, S, R>(
        topology: T,
        chunk_client: C,
        store: S,
        reserve: R,
    ) -> StorerComponents<T, C, S, R> {
        StorerComponents::new(topology, chunk_client, store, reserve)
    }
}
