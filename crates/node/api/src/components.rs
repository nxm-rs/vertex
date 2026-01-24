//! Runtime component instances for nodes.
//!
//! Simple containers bridging SwarmTypes with runtime instances.

use vertex_swarm_api::{FullTypes, LightTypes, PublisherTypes, SwarmReader, SwarmWriter};

/// Components for a light node (read-only).
#[derive(Debug, Clone)]
pub struct LightComponents<Types: LightTypes, S: SwarmReader<Types>> {
    pub swarm: S,
    pub topology: Types::Topology,
}

impl<Types: LightTypes, S: SwarmReader<Types>> LightComponents<Types, S> {
    pub fn new(swarm: S, topology: Types::Topology) -> Self {
        Self { swarm, topology }
    }
}

/// Components for a publisher node (can upload).
#[derive(Debug, Clone)]
pub struct PublisherComponents<Types: PublisherTypes, S: SwarmWriter<Types>> {
    pub swarm: S,
    pub topology: Types::Topology,
}

impl<Types: PublisherTypes, S: SwarmWriter<Types>> PublisherComponents<Types, S> {
    pub fn new(swarm: S, topology: Types::Topology) -> Self {
        Self { swarm, topology }
    }
}

/// Components for a full node (stores and syncs).
#[derive(Debug, Clone)]
pub struct FullComponents<Types: FullTypes, S: SwarmWriter<Types>> {
    pub swarm: S,
    pub topology: Types::Topology,
    pub store: Types::Store,
    pub sync: Types::Sync,
}

impl<Types: FullTypes, S: SwarmWriter<Types>> FullComponents<Types, S> {
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
