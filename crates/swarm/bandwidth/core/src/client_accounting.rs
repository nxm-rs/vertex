//! Combined pricing and bandwidth accounting.

use std::sync::Arc;

use nectar_primitives::ChunkAddress;
use vertex_swarm_api::{SwarmBandwidthAccounting, SwarmResult, SwarmSpec};
use vertex_swarm_primitives::OverlayAddress;

use crate::pricing;

/// Combined pricing and bandwidth accounting for client operations.
#[derive(Clone)]
pub struct ClientAccounting<B, S> {
    bandwidth: B,
    spec: Arc<S>,
}

impl<B, S> ClientAccounting<B, S> {
    /// Create a new client accounting instance.
    pub fn new(bandwidth: B, spec: Arc<S>) -> Self {
        Self { bandwidth, spec }
    }

    /// Decompose into parts.
    pub fn into_parts(self) -> (B, Arc<S>) {
        (self.bandwidth, self.spec)
    }

    /// Get the bandwidth accounting.
    pub fn bandwidth(&self) -> &B {
        &self.bandwidth
    }

    /// Get the spec.
    pub fn spec(&self) -> &S {
        &self.spec
    }
}

impl<B, S> ClientAccounting<B, S>
where
    B: SwarmBandwidthAccounting,
    S: SwarmSpec,
{
    /// Calculate chunk price for a peer.
    pub fn chunk_price(&self, peer: &OverlayAddress, chunk: &ChunkAddress) -> u64 {
        pricing::chunk_price(&self.spec, peer, chunk)
    }

    /// Prepare to receive a chunk (we pay, balance decreases).
    pub fn prepare_receive_chunk(
        &self,
        peer: OverlayAddress,
        chunk: &ChunkAddress,
        originated: bool,
    ) -> SwarmResult<B::Action> {
        let price = pricing::chunk_price(&self.spec, &peer, chunk);
        self.bandwidth.prepare_receive(peer, price, originated)
    }

    /// Prepare to provide a chunk (peer pays, balance increases).
    pub fn prepare_provide_chunk(
        &self,
        peer: OverlayAddress,
        chunk: &ChunkAddress,
    ) -> SwarmResult<B::Action> {
        let price = pricing::chunk_price(&self.spec, &peer, chunk);
        self.bandwidth.prepare_provide(peer, price)
    }

    /// Get or create accounting for a peer.
    pub fn for_peer(&self, peer: OverlayAddress) -> B::Peer {
        self.bandwidth.for_peer(peer)
    }
}
