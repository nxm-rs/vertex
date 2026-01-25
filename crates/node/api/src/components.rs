//! Runtime component instances for nodes.
//!
//! Simple containers bridging SwarmTypes with runtime instances.

use vertex_swarm_api::{FullTypes, LightTypes, PublisherTypes, SwarmReader, SwarmWriter};

/// Components for a light node (read-only).
#[derive(Debug, Clone)]
pub struct LightComponents<Types: LightTypes, S: SwarmReader> {
    /// Swarm reader for chunk retrieval.
    pub swarm: S,
    /// Network topology manager.
    pub topology: Types::Topology,
}

impl<Types: LightTypes, S: SwarmReader> LightComponents<Types, S> {
    /// Create new light node components.
    pub fn new(swarm: S, topology: Types::Topology) -> Self {
        Self { swarm, topology }
    }
}

/// Components for a publisher node (can upload).
#[derive(Debug, Clone)]
pub struct PublisherComponents<Types: PublisherTypes, S: SwarmWriter> {
    /// Swarm writer for chunk upload and retrieval.
    pub swarm: S,
    /// Network topology manager.
    pub topology: Types::Topology,
}

impl<Types: PublisherTypes, S: SwarmWriter> PublisherComponents<Types, S> {
    /// Create new publisher node components.
    pub fn new(swarm: S, topology: Types::Topology) -> Self {
        Self { swarm, topology }
    }
}

/// Components for a full node (stores and syncs).
#[derive(Debug, Clone)]
pub struct FullComponents<Types: FullTypes, S: SwarmWriter> {
    /// Swarm writer for chunk upload and retrieval.
    pub swarm: S,
    /// Network topology manager.
    pub topology: Types::Topology,
    /// Local chunk store.
    pub store: Types::Store,
    /// Chunk synchronization.
    pub sync: Types::Sync,
}

impl<Types: FullTypes, S: SwarmWriter> FullComponents<Types, S> {
    /// Create new full node components.
    pub fn new(
        swarm: S,
        topology: Types::Topology,
        store: Types::Store,
        sync: Types::Sync,
    ) -> Self {
        Self {
            swarm,
            topology,
            store,
            sync,
        }
    }
}
