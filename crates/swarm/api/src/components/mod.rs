//! Component containers for Swarm node capabilities.

mod bandwidth;
mod localstore;
mod pricing;
mod topology;

pub use bandwidth::*;
pub use localstore::*;
pub use pricing::*;
pub use topology::*;

/// Uniform topology access across component levels.
pub trait HasTopology {
    /// The topology type.
    type Topology;
    /// Get the topology.
    fn topology(&self) -> &Self::Topology;
}

/// Uniform accounting access (client/storer levels).
pub trait HasAccounting {
    /// The accounting type.
    type Accounting;
    /// Get the accounting.
    fn accounting(&self) -> &Self::Accounting;
}

/// Uniform store access (storer level).
pub trait HasStore {
    /// The store type.
    type Store;
    /// Get the local store.
    fn store(&self) -> &Self::Store;
}

/// Bootnode components (topology only). Identity via `topology.identity()`.
#[derive(Debug)]
pub struct BootnodeComponents<T> {
    /// Network topology.
    pub topology: T,
}

impl<T> BootnodeComponents<T> {
    /// Create bootnode components.
    pub fn new(topology: T) -> Self {
        Self { topology }
    }
}

impl<T> HasTopology for BootnodeComponents<T> {
    type Topology = T;
    fn topology(&self) -> &T {
        &self.topology
    }
}

/// Client components (topology + accounting). Can retrieve/upload chunks.
#[derive(Debug)]
pub struct ClientComponents<T, A> {
    /// Base bootnode components.
    pub base: BootnodeComponents<T>,
    /// Combined pricing and bandwidth accounting.
    pub accounting: A,
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

impl<T, A> HasTopology for ClientComponents<T, A> {
    type Topology = T;
    fn topology(&self) -> &T {
        self.base.topology()
    }
}

impl<T, A> HasAccounting for ClientComponents<T, A> {
    type Accounting = A;
    fn accounting(&self) -> &A {
        &self.accounting
    }
}

/// Storer components (client + local store). Stores chunks locally.
#[derive(Debug)]
pub struct StorerComponents<T, A, S> {
    /// Client-level components.
    pub client: ClientComponents<T, A>,
    /// Local chunk storage.
    pub store: S,
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

impl<T, A, S> HasTopology for StorerComponents<T, A, S> {
    type Topology = T;
    fn topology(&self) -> &T {
        self.client.topology()
    }
}

impl<T, A, S> HasAccounting for StorerComponents<T, A, S> {
    type Accounting = A;
    fn accounting(&self) -> &A {
        self.client.accounting()
    }
}

impl<T, A, S> HasStore for StorerComponents<T, A, S> {
    type Store = S;
    fn store(&self) -> &S {
        &self.store
    }
}
