//! Pseudosettle settlement boundary types.
//!
//! The crate's own inbound/outbound message shapes. Keeping the boundary local
//! lets the provider build without a node crate, so the crate stays compilable
//! for `wasm32-unknown-unknown`.

use alloy_primitives::U256;
use vertex_swarm_net_pseudosettle::PaymentAck;
use vertex_swarm_primitives::OverlayAddress;

/// A pseudosettle event routed to the service from the network layer.
#[derive(Debug, Clone)]
pub enum PseudosettleEvent {
    /// We sent a pseudosettle and the peer acked it.
    Sent {
        /// The peer we settled with.
        peer: OverlayAddress,
        /// The acknowledgment the peer returned.
        ack: PaymentAck,
    },
    /// A peer sent us a pseudosettle request.
    Received {
        /// The peer that sent the request.
        peer: OverlayAddress,
        /// The payment amount the peer requested, in wire units.
        amount: U256,
        /// Request identifier used to address the ack back to the peer.
        request_id: u64,
    },
}

/// An outbound pseudosettle command the service hands to the network layer.
#[derive(Debug, Clone)]
pub enum PseudosettleNetworkCommand {
    /// Send a pseudosettle payment to a peer.
    Send {
        /// The peer to pay.
        peer: OverlayAddress,
        /// The amount to pay, in wire units.
        amount: U256,
    },
    /// Acknowledge a pseudosettle request from a peer.
    Ack {
        /// The peer to ack.
        peer: OverlayAddress,
        /// Request identifier from the received payment.
        request_id: u64,
        /// The ack to return.
        ack: PaymentAck,
    },
}
