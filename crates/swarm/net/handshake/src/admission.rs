//! Handshake admission control.
//!
//! Routing (or any other gate) can veto a handshake after the peer's
//! identity is verified but before the local side commits to the final
//! message of the exchange. The handshake crate stays free of any
//! routing or topology dependency: it defines the trait, accepts a
//! dyn-erased implementation through
//! [`HandshakeBehaviour::with_admission_control`](crate::HandshakeBehaviour::with_admission_control),
//! and consults it from inside the protocol.
//!
//! # Asymmetry
//!
//! The 3-message SYN, SYNACK, ACK exchange exposes the peer's identity
//! to each side at a different step:
//!
//! * the outbound side learns the remote identity from SYNACK, so it
//!   evaluates admission before sending ACK; rejection aborts cleanly
//!   without committing to the exchange,
//! * the inbound side learns the remote identity only from ACK, so it
//!   can only evaluate after the outbound side has already sent ACK
//!   and closed its half of the stream.
//!
//! When the inbound side rejects, the outbound side observes a
//! successful handshake immediately followed by a transport-level
//! disconnect. Only the locally-rejecting side surfaces
//! [`HandshakeError::AdmissionRejected`](crate::HandshakeError::AdmissionRejected);
//! the other side surfaces whatever close or timeout error its
//! framing layer produces. Topology should treat post-handshake
//! disconnects from the remote as a normal disconnect and apply the
//! usual backoff.

use std::sync::Arc;

use vertex_swarm_peer::{SwarmAddress, SwarmNodeType};

pub use vertex_net_peer_registry::ConnectionDirection;

/// Reason a peer was rejected from completing the handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum AdmissionRejection {
    /// Routing capacity is full for this peer's proximity bin.
    Saturated,
    /// Peer is on the blocklist.
    Blocklisted,
    /// Neighborhood is oversaturated and this peer falls outside the
    /// configured headroom.
    OversaturatedNeighborhood,
}

/// Result of evaluating a peer for handshake admission.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum AdmissionDecision {
    /// Allow the handshake to complete.
    Accept,
    /// Reject the handshake. The protocol aborts before committing to
    /// the final message and the handler surfaces
    /// [`HandshakeError::AdmissionRejected`](crate::HandshakeError::AdmissionRejected)
    /// with this reason.
    Reject(AdmissionRejection),
}

/// Admission control gate consulted before the local side commits to
/// the final handshake message.
///
/// Implementations must be cheap to call: `evaluate` runs on the hot
/// path of every handshake. Trait is dyn-compatible so it can live
/// behind [`SharedAdmissionControl`].
pub trait HandshakeAdmissionControl: Send + Sync + 'static {
    /// Evaluate a peer that just identified itself during the handshake.
    fn evaluate(
        &self,
        peer_overlay: &SwarmAddress,
        node_type: SwarmNodeType,
        direction: ConnectionDirection,
    ) -> AdmissionDecision;
}

/// Always-accept admission control. Default when no gate is wired in.
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

/// Default admission control: accept everyone. Returned as the
/// type-erased handle the behaviour stores.
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
