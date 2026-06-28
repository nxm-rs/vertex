//! No-op implementations that always allow and never settle.

use std::vec::Vec;

use vertex_swarm_api::{
    Au, Direction, SwarmBandwidthAccounting, SwarmIdentity, SwarmPeerBandwidth, SwarmResult,
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

impl SwarmPeerBandwidth for NoPeerBandwidth {
    fn record(&self, _amount: Au, _direction: Direction) {}

    fn balance(&self) -> Au {
        Au::ZERO
    }

    async fn settle(&self) -> SwarmResult<()> {
        Ok(())
    }

    fn peer(&self) -> OverlayAddress {
        self.peer
    }
}

/// No-op receive action (does nothing on apply).
pub struct NoReceiveAction;

/// No-op provide action (does nothing on apply).
pub struct NoProvideAction;

impl vertex_swarm_api::AccountingAction for NoReceiveAction {
    fn apply(self) {}

    fn apply_boxed(self: Box<Self>) {}
}

impl vertex_swarm_api::AccountingAction for NoProvideAction {
    fn apply(self) {}

    fn apply_boxed(self: Box<Self>) {}
}

impl<I: SwarmIdentity> SwarmBandwidthAccounting for NoAccounting<I> {
    type Identity = I;
    type Peer = NoPeerBandwidth;
    type ReceiveAction = NoReceiveAction;
    type ProvideAction = NoProvideAction;

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

    fn prepare_receive(
        &self,
        _peer: OverlayAddress,
        _price: Au,
        _originated: bool,
    ) -> SwarmResult<NoReceiveAction> {
        Ok(NoReceiveAction)
    }

    fn prepare_provide(&self, _peer: OverlayAddress, _price: Au) -> SwarmResult<NoProvideAction> {
        Ok(NoProvideAction)
    }
}
