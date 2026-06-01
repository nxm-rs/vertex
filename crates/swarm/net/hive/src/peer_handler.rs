//! Dispatch policy for inbound peer batches.

/// What the protocol reader should do with a fresh inbound peer batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum InboundPolicy {
    /// Validate the batch and surface it as a
    /// [`HiveEvent::PeersReceived`](crate::HiveEvent::PeersReceived) so the
    /// topology layer can verify and route the peers.
    Forward,
    /// Drop the batch without running validation. The protocol reader bumps
    /// `hive_peers_discarded_total{reason="bootnode_mode"}` on the raw wire
    /// count and returns an empty Vec, so no event is emitted downstream.
    Discard,
}

/// Strategy for handling inbound peer batches.
pub trait HivePeerHandler: Send + Sync + 'static {
    fn policy(&self) -> InboundPolicy;
}

/// Default handler for non-bootnode roles: forward batches to the topology.
#[derive(Debug, Default, Clone, Copy)]
pub struct LearnAndDial;

impl HivePeerHandler for LearnAndDial {
    fn policy(&self) -> InboundPolicy {
        InboundPolicy::Forward
    }
}

/// Bootnode handler: discard inbound batches without ECDSA validation.
#[derive(Debug, Default, Clone, Copy)]
pub struct DiscardSilently;

impl HivePeerHandler for DiscardSilently {
    fn policy(&self) -> InboundPolicy {
        InboundPolicy::Discard
    }
}
