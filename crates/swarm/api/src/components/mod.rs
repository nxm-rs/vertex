//! Swarm node components - traits and runtime containers.
//!
//! This module contains:
//! - Component traits ([`Topology`], [`BandwidthAccounting`], [`LocalStore`], [`ChunkSync`])
//! - Runtime component containers ([`SwarmBaseComponents`], [`SwarmLightComponents`], etc.)

mod bandwidth;
mod store;
mod sync;
mod topology;

pub use bandwidth::*;
pub use store::*;
pub use sync::*;
pub use topology::*;

use crate::{BootnodeTypes, FullTypes, LightTypes, PublisherTypes};

/// Base components for all Swarm nodes.
///
/// Contains the fundamental components needed for network participation.
#[derive(Debug, Clone)]
pub struct SwarmBaseComponents<Types: BootnodeTypes>
where
    Types::Identity: Clone,
    Types::Topology: Clone,
{
    /// The node's cryptographic identity.
    pub identity: Types::Identity,

    /// Network topology manager (peer discovery, routing).
    pub topology: Types::Topology,
}

impl<Types: BootnodeTypes> SwarmBaseComponents<Types>
where
    Types::Identity: Clone,
    Types::Topology: Clone,
{
    /// Create new base components.
    pub fn new(identity: Types::Identity, topology: Types::Topology) -> Self {
        Self { identity, topology }
    }
}

/// Runtime components of a built light Swarm node.
///
/// Light nodes can retrieve chunks but cannot store or upload them.
/// Composes base components with bandwidth accounting.
#[derive(Debug, Clone)]
pub struct SwarmLightComponents<Types: LightTypes>
where
    Types::Identity: Clone,
    Types::Topology: Clone,
    Types::Accounting: Clone,
{
    /// Base components (identity, topology).
    pub base: SwarmBaseComponents<Types>,

    /// Bandwidth accounting for availability incentives.
    pub accounting: Types::Accounting,
}

impl<Types: LightTypes> SwarmLightComponents<Types>
where
    Types::Identity: Clone,
    Types::Topology: Clone,
    Types::Accounting: Clone,
{
    /// Create new light node components.
    pub fn new(
        identity: Types::Identity,
        topology: Types::Topology,
        accounting: Types::Accounting,
    ) -> Self {
        Self {
            base: SwarmBaseComponents::new(identity, topology),
            accounting,
        }
    }

    /// Get the node's identity.
    pub fn identity(&self) -> &Types::Identity {
        &self.base.identity
    }

    /// Get the topology manager.
    pub fn topology(&self) -> &Types::Topology {
        &self.base.topology
    }
}

/// Runtime components of a built publisher Swarm node.
///
/// Publisher nodes can retrieve and upload chunks but don't store them locally.
/// Composes light components with storage proof.
#[derive(Debug, Clone)]
pub struct SwarmPublisherComponents<Types: PublisherTypes>
where
    Types::Identity: Clone,
    Types::Topology: Clone,
    Types::Accounting: Clone,
    Types::Storage: Clone,
{
    /// Light node components (base + accounting).
    pub light: SwarmLightComponents<Types>,

    /// Storage proof for uploads (postage stamps).
    pub storage: Types::Storage,
}

impl<Types: PublisherTypes> SwarmPublisherComponents<Types>
where
    Types::Identity: Clone,
    Types::Topology: Clone,
    Types::Accounting: Clone,
    Types::Storage: Clone,
{
    /// Create new publisher node components.
    pub fn new(
        identity: Types::Identity,
        topology: Types::Topology,
        accounting: Types::Accounting,
        storage: Types::Storage,
    ) -> Self {
        Self {
            light: SwarmLightComponents::new(identity, topology, accounting),
            storage,
        }
    }

    /// Get the node's identity.
    pub fn identity(&self) -> &Types::Identity {
        self.light.identity()
    }

    /// Get the topology manager.
    pub fn topology(&self) -> &Types::Topology {
        self.light.topology()
    }

    /// Get the accounting manager.
    pub fn accounting(&self) -> &Types::Accounting {
        &self.light.accounting
    }
}

/// Runtime components of a built full Swarm node.
///
/// Full nodes store chunks locally and sync with neighbors.
/// Composes publisher components with local store and sync.
#[derive(Debug, Clone)]
pub struct SwarmFullComponents<Types: FullTypes>
where
    Types::Identity: Clone,
    Types::Topology: Clone,
    Types::Accounting: Clone,
    Types::Storage: Clone,
    Types::Store: Clone,
    Types::Sync: Clone,
{
    /// Publisher node components (light + storage).
    pub publisher: SwarmPublisherComponents<Types>,

    /// Local chunk storage.
    pub store: Types::Store,

    /// Chunk synchronization with neighbors.
    pub sync: Types::Sync,
}

impl<Types: FullTypes> SwarmFullComponents<Types>
where
    Types::Identity: Clone,
    Types::Topology: Clone,
    Types::Accounting: Clone,
    Types::Storage: Clone,
    Types::Store: Clone,
    Types::Sync: Clone,
{
    /// Create new full node components.
    pub fn new(
        identity: Types::Identity,
        topology: Types::Topology,
        accounting: Types::Accounting,
        storage: Types::Storage,
        store: Types::Store,
        sync: Types::Sync,
    ) -> Self {
        Self {
            publisher: SwarmPublisherComponents::new(identity, topology, accounting, storage),
            store,
            sync,
        }
    }

    /// Get the node's identity.
    pub fn identity(&self) -> &Types::Identity {
        self.publisher.identity()
    }

    /// Get the topology manager.
    pub fn topology(&self) -> &Types::Topology {
        self.publisher.topology()
    }

    /// Get the accounting manager.
    pub fn accounting(&self) -> &Types::Accounting {
        self.publisher.accounting()
    }

    /// Get the storage proof.
    pub fn storage(&self) -> &Types::Storage {
        &self.publisher.storage
    }
}
