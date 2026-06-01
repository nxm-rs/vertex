//! Handshake admission control.
//!
//! Lets routing (or any other gate) veto a handshake after the peer's
//! identity is known, but before the final Ack is sent. Mirrors bee's
//! `p2p.Picker` (`bee/pkg/p2p/libp2p/internal/handshake/handshake.go:167`).
//!
//! The handshake crate stays free of any routing/topology dependency: it
//! defines the trait and accepts a dyn-erased implementation via
//! [`HandshakeBehaviour::with_admission_control`](crate::HandshakeBehaviour::with_admission_control).

use std::sync::Arc;

use libp2p::PeerId;
use vertex_swarm_peer::{SwarmAddress, SwarmNodeType};

pub use vertex_net_peer_registry::ConnectionDirection;

/// Reason a peer was rejected from completing the handshake.
///
/// `#[non_exhaustive]` so adding new reasons is non-breaking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum AdmissionRejection {
    /// Routing capacity is full for this peer's proximity bin.
    Saturated,
    /// Peer is on the blocklist (banned).
    Blocklisted,
    /// Neighborhood is oversaturated and this peer falls outside the
    /// configured headroom.
    OversaturatedNeighborhood,
}

/// Why an existing peer is being evicted to make room for an incoming one.
///
/// `#[non_exhaustive]` so new eviction reasons can be added without breaking
/// downstream consumers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum EvictReason {
    /// Incoming peer is closer (higher proximity order) than the evictee.
    Oversaturated,
}

/// Result of evaluating a peer for handshake admission.
///
/// `#[non_exhaustive]` so adding new outcomes (e.g. defer/retry) is
/// non-breaking.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum AdmissionDecision {
    /// Allow the handshake to complete.
    Accept,
    /// Reject the handshake. The handshake will abort before the final Ack
    /// and the handler reports [`HandshakeError::AdmissionRejected`](crate::HandshakeError::AdmissionRejected).
    Reject(AdmissionRejection),
    /// Accept the new peer but evict an existing one first.
    ///
    /// The handshake crate does not perform eviction; it just surfaces the
    /// decision. Unit 8 wires the eviction path through the topology
    /// behaviour.
    AcceptEvict {
        evict: PeerId,
        reason: EvictReason,
    },
}

/// Admission control gate consulted before the final handshake Ack.
///
/// Implementors must be cheap to call: it runs on every handshake completion.
/// Trait is dyn-compatible so it can live behind `Arc<dyn ...>` in the
/// handshake behaviour.
pub trait HandshakeAdmissionControl: Send + Sync + 'static {
    /// Evaluate a peer that just identified itself during the handshake.
    fn evaluate(
        &self,
        peer_overlay: &SwarmAddress,
        node_type: SwarmNodeType,
        direction: ConnectionDirection,
    ) -> AdmissionDecision;
}

/// Always-accept admission control. Used as the default when none is wired in.
#[derive(Debug, Clone, Copy, Default)]
pub struct AlwaysAccept;

impl HandshakeAdmissionControl for AlwaysAccept {
    fn evaluate(
        &self,
        _peer_overlay: &SwarmAddress,
        _node_type: SwarmNodeType,
        _direction: ConnectionDirection,
    ) -> AdmissionDecision {
        AdmissionDecision::Accept
    }
}

impl<T: HandshakeAdmissionControl + ?Sized> HandshakeAdmissionControl for Arc<T> {
    fn evaluate(
        &self,
        peer_overlay: &SwarmAddress,
        node_type: SwarmNodeType,
        direction: ConnectionDirection,
    ) -> AdmissionDecision {
        (**self).evaluate(peer_overlay, node_type, direction)
    }
}

/// Type-erased shared handle to an admission controller.
pub type SharedAdmissionControl = Arc<dyn HandshakeAdmissionControl>;

/// Default admission control: accept everyone. Lives behind an `Arc` to match
/// the handshake behaviour's slot.
pub fn default_admission_control() -> SharedAdmissionControl {
    Arc::new(AlwaysAccept)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn always_accept_returns_accept() {
        let ac = AlwaysAccept;
        let decision = ac.evaluate(
            &SwarmAddress::with_first_byte(0xaa),
            SwarmNodeType::Storer,
            ConnectionDirection::Inbound,
        );
        assert!(matches!(decision, AdmissionDecision::Accept));
    }

    #[test]
    fn arc_delegates() {
        let ac: SharedAdmissionControl = default_admission_control();
        let decision = ac.evaluate(
            &SwarmAddress::with_first_byte(0x01),
            SwarmNodeType::Client,
            ConnectionDirection::Outbound,
        );
        assert!(matches!(decision, AdmissionDecision::Accept));
    }
}
