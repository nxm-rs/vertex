//! Component containers and access traits for Swarm nodes.

mod bandwidth;
mod localstore;
mod peers;
mod topology;

pub use self::bandwidth::{
    BandwidthMode, Direction, SwarmAccountingConfig, SwarmBandwidthAccounting, SwarmPeerAccounting,
    SwarmPeerBandwidth, SwarmSettlementProvider,
};
pub use self::localstore::{SwarmLocalStore, SwarmLocalStoreConfig};
pub use self::peers::{SwarmPeerRegistry, SwarmPeerResolver, SwarmPeerStore, SwarmScoreStore};
pub use self::topology::{
    SwarmTopology, SwarmTopologyBins, SwarmTopologyCommands, SwarmTopologyPeers,
    SwarmTopologyRouting, SwarmTopologyState, SwarmTopologyStats,
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

/// Identity access.
pub trait HasIdentity: Send + Sync {
    /// The identity type.
    type Identity: SwarmIdentity;

    /// Get the identity.
    fn identity(&self) -> &Self::Identity;
}

/// Accounting access (client/storer levels).
#[auto_impl::auto_impl(&, Arc, Box)]
pub trait HasAccounting: Send + Sync {
    /// The accounting type.
    type Accounting: Send + Sync;

    /// Get the accounting.
    fn accounting(&self) -> &Self::Accounting;
}

/// Local store access (storer level).
#[auto_impl::auto_impl(&, Arc, Box)]
pub trait HasStore: Send + Sync {
    /// The store type.
    type Store: Send + Sync;

    /// Get the local store.
    fn store(&self) -> &Self::Store;
}

/// Bootnode components (topology only). Identity via `topology().identity()`.
#[derive(Debug)]
pub struct BootnodeComponents<T> {
    topology: T,
}

impl<T> BootnodeComponents<T> {
    /// Create bootnode components.
    pub fn new(topology: T) -> Self {
        Self { topology }
    }
}

impl<T: Send + Sync> HasTopology for BootnodeComponents<T> {
    type Topology = T;

    fn topology(&self) -> &T {
        &self.topology
    }
}

/// Client components (topology + accounting).
#[derive(Debug)]
pub struct ClientComponents<T, A> {
    base: BootnodeComponents<T>,
    accounting: A,
}

impl<T, A> ClientComponents<T, A> {
    /// Create client components.
    pub fn new(topology: T, accounting: A) -> Self {
        Self {
            base: BootnodeComponents::new(topology),
            accounting,
        }
    }

    /// Create from existing bootnode components.
    pub fn from_base(base: BootnodeComponents<T>, accounting: A) -> Self {
        Self { base, accounting }
    }
}

impl<T: Send + Sync, A: Send + Sync> HasTopology for ClientComponents<T, A> {
    type Topology = T;

    fn topology(&self) -> &T {
        self.base.topology()
    }
}

impl<T: Send + Sync, A: Send + Sync> HasAccounting for ClientComponents<T, A> {
    type Accounting = A;

    fn accounting(&self) -> &A {
        &self.accounting
    }
}

/// Storer components (client + local store).
#[derive(Debug)]
pub struct StorerComponents<T, A, S> {
    client: ClientComponents<T, A>,
    store: S,
}

impl<T, A, S> StorerComponents<T, A, S> {
    /// Create storer components.
    pub fn new(topology: T, accounting: A, store: S) -> Self {
        Self {
            client: ClientComponents::new(topology, accounting),
            store,
        }
    }

    /// Create from existing client components.
    pub fn from_client(client: ClientComponents<T, A>, store: S) -> Self {
        Self { client, store }
    }
}

impl<T: Send + Sync, A: Send + Sync, S: Send + Sync> HasTopology for StorerComponents<T, A, S> {
    type Topology = T;

    fn topology(&self) -> &T {
        self.client.topology()
    }
}

impl<T: Send + Sync, A: Send + Sync, S: Send + Sync> HasAccounting for StorerComponents<T, A, S> {
    type Accounting = A;

    fn accounting(&self) -> &A {
        self.client.accounting()
    }
}

impl<T: Send + Sync, A: Send + Sync, S: Send + Sync> HasStore for StorerComponents<T, A, S> {
    type Store = S;

    fn store(&self) -> &S {
        &self.store
    }
}
