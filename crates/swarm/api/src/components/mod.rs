//! Component containers and access traits for Swarm nodes.

mod bandwidth;
mod localstore;
mod peers;
mod pricing;
mod reserve;
mod topology;

pub use self::bandwidth::{
    AccountingAction, BandwidthMode, Direction, SwarmAccountingConfig, SwarmBandwidthAccounting,
    SwarmClientAccounting, SwarmPeerBandwidth, SwarmPeerState, SwarmSettlementProvider,
};
pub use self::localstore::{SwarmLocalStore, SwarmLocalStoreConfig};
pub use self::peers::SwarmPeerResolver;
pub use self::pricing::{SwarmPricing, SwarmPricingBuilder, SwarmPricingConfig};
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

/// Chunk client access (client/storer levels).
///
/// Exposes the components' chunk client so the gRPC chunk service and embedders
/// (FFI) can drive uploads and downloads. Mirrors [`HasTopology`].
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
/// Narrows [`HasStore`] to the storer reserve: the proximity-ordered,
/// always-stamped [`ReserveStore`]. A storer that runs a reserve exposes it here
/// so the redistribution and sync subsystems can query radius, capacity, and
/// per-bin insertion order without depending on the concrete store type.
#[auto_impl::auto_impl(&, Arc, Box)]
pub trait HasReserve: HasStore {
    /// The reserve type.
    ///
    /// Bounded by [`BinCursorStore`] (which refines [`ReserveStore`]) so the
    /// wired handle exposes both the proximity axis and the per-bin
    /// insertion-order axis the redistribution sampler and sync need; without it
    /// the cursor surface would be unreachable behind the erased handle.
    type Reserve: BinCursorStore;

    /// Get the reserve.
    fn reserve(&self) -> &Self::Reserve;
}

/// Bootnode components (topology only). Identity via `topology().identity()`.
///
/// Construction is builder-exclusive; see [`construct`].
#[derive(Debug)]
pub struct BootnodeComponents<T> {
    topology: T,
}

impl<T> BootnodeComponents<T> {
    /// Wire bootnode components. Crate-visible: only the builder constructs
    /// components, via the [`construct`] seam.
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
/// The transport-facing read surface a built client/storer exposes: topology
/// for the node service, chunk client for the chunk service.
///
/// Accounting is deliberately not a component. It is a builder-wired internal of
/// the network chunk client — the network chunk provider settles through a
/// shared accounting `Arc` plumbed in at launch — so it never surfaces as a
/// served capability. Local (non-network) providers do not account at all, and
/// bootnodes run only a listen-only pricing handler (no accounting state).
///
/// Construction is builder-exclusive; see [`construct`].
#[derive(Debug, Clone)]
pub struct ClientComponents<T, C> {
    topology: T,
    chunk_client: C,
}

impl<T, C> ClientComponents<T, C> {
    /// Wire client components. Crate-visible: only the builder constructs
    /// components, via the [`construct`] seam.
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

/// Storer components (client + local store).
///
/// Construction is builder-exclusive; see [`construct`].
#[derive(Debug)]
pub struct StorerComponents<T, C, S> {
    client: ClientComponents<T, C>,
    store: S,
}

impl<T, C, S> StorerComponents<T, C, S> {
    /// Wire storer components. Crate-visible: only the builder constructs
    /// components, via the [`construct`] seam.
    #[expect(dead_code, reason = "storer wiring lands with the storer extension")]
    pub(crate) fn new(topology: T, chunk_client: C, store: S) -> Self {
        Self {
            client: ClientComponents::new(topology, chunk_client),
            store,
        }
    }
}

impl<T: Send + Sync, C: Send + Sync, S: Send + Sync> HasTopology for StorerComponents<T, C, S> {
    type Topology = T;

    fn topology(&self) -> &T {
        self.client.topology()
    }
}

impl<T: Send + Sync, C: Send + Sync, S: Send + Sync> HasChunkClient for StorerComponents<T, C, S> {
    type ChunkClient = C;

    fn chunk_client(&self) -> &C {
        self.client.chunk_client()
    }
}

impl<T: Send + Sync, C: Send + Sync, S: Send + Sync> HasStore for StorerComponents<T, C, S> {
    type Store = S;

    fn store(&self) -> &S {
        &self.store
    }
}

/// Builder-only construction seam for the component containers.
///
/// Components are a builder-wired provider DAG over shared `Arc`s (the chunk
/// provider already embeds the topology handle); only the builder wires those
/// `Arc`s correctly, so the public containers expose no constructors. The
/// builder and the in-process node reach construction through these
/// `#[doc(hidden)]` free functions instead. Not part of the stable API.
#[doc(hidden)]
pub mod construct {
    use super::{BootnodeComponents, ClientComponents};

    /// Wire [`BootnodeComponents`].
    pub fn bootnode<T>(topology: T) -> BootnodeComponents<T> {
        BootnodeComponents::new(topology)
    }

    /// Wire [`ClientComponents`].
    pub fn client<T, C>(topology: T, chunk_client: C) -> ClientComponents<T, C> {
        ClientComponents::new(topology, chunk_client)
    }
}
