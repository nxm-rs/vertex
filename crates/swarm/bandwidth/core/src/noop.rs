//! No-op implementations that always allow and never settle.

use std::vec::Vec;

use vertex_swarm_api::{
    Direction, SwarmBandwidthAccounting, SwarmIdentity, SwarmPeerBandwidth, SwarmResult,
};
use vertex_swarm_primitives::OverlayAddress;

/// No-op bandwidth accounting (always allows, never settles).
#[derive(Debug, Clone)]
pub struct NoAccounting<I: SwarmIdentity> {
    identity: I,
}

impl<I: SwarmIdentity> NoAccounting<I> {
    /// Create a new no-op accounting with the given identity.
    pub fn new(identity: I) -> Self {
        Self { identity }
    }
}

/// No-op per-peer bandwidth handle.
#[derive(Debug, Clone)]
pub struct NoPeerBandwidth {
    peer: OverlayAddress,
}

#[async_trait::async_trait]
impl SwarmPeerBandwidth for NoPeerBandwidth {
    fn record(&self, _bytes: u64, _direction: Direction) {}

    fn allow(&self, _bytes: u64) -> bool {
        true
    }

    fn balance(&self) -> i64 {
        0
    }

    async fn settle(&self) -> SwarmResult<()> {
        Ok(())
    }

    fn peer(&self) -> OverlayAddress {
        self.peer
    }
}

impl<I: SwarmIdentity> SwarmBandwidthAccounting for NoAccounting<I> {
    type Identity = I;
    type Peer = NoPeerBandwidth;

    fn identity(&self) -> &I {
        &self.identity
    }

    fn for_peer(&self, peer: OverlayAddress) -> Self::Peer {
        NoPeerBandwidth { peer }
    }

    fn peers(&self) -> Vec<OverlayAddress> {
        Vec::new()
    }

    fn remove_peer(&self, _peer: &OverlayAddress) {}
}
