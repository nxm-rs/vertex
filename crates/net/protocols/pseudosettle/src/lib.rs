//! Pseudosettle protocol for Swarm bandwidth accounting.
//!
//! This crate provides the wire protocol for exchanging pseudosettle payments
//! between peers. It is **pure protocol plumbing** - it does NOT make business
//! decisions about settlements.
//!
//! # Protocol
//!
//! - Path: `/swarm/pseudosettle/1.0.0/pseudosettle`
//! - Pattern: Request/Response with typed messages
//! - Request: `Payment { amount }` - big-endian bytes (leading zeros trimmed)
//! - Response: `PaymentAck { amount, timestamp }` - amount confirmed, nanosecond timestamp
//!
//! # Typed Message Exchange
//!
//! This protocol uses **separate typed codecs** for request and response:
//! - `PaymentCodec` - Encodes/decodes `Payment` messages only
//! - `PaymentAckCodec` - Encodes/decodes `PaymentAck` messages only
//!
//! This enforces type safety at the protocol level - the initiator can only
//! send `Payment` and receive `PaymentAck`, and the responder does the opposite.
//!
//! # Timestamp Validation
//!
//! The responder's timestamp should be validated to be within +-2-3 seconds of
//! the local time. Use [`validate_timestamp`] for this check.
//!
//! # Usage
//!
//! ## Initiating a settlement (outbound)
//!
//! ```ignore
//! use vertex_net_pseudosettle::{Payment, outbound, validate_timestamp};
//!
//! let payment = Payment::from_u64(1_000_000);
//! let protocol = outbound(payment);
//!
//! // After negotiation...
//! let ack: PaymentAck = protocol.upgrade_outbound(...).await?;
//!
//! // Validate timestamp is recent
//! validate_timestamp(ack.timestamp, 3)?;
//! ```
//!
//! ## Handling incoming settlement (inbound)
//!
//! ```ignore
//! use vertex_net_pseudosettle::{PaymentAck, inbound};
//!
//! let protocol = inbound();
//!
//! // After negotiation...
//! let result = protocol.upgrade_inbound(...).await?;
//!
//! // Process payment and respond (type-safe - can only send PaymentAck)
//! result.ack_now().await?;
//! // Or with custom amount/timestamp:
//! // result.respond(PaymentAck::new(amount, timestamp)).await?;
//! ```
//!
//! # Business Logic (NOT in this crate)
//!
//! - Whether to accept a settlement request
//! - Balance tracking and updates
//! - Settlement threshold decisions
//! - Peer disconnection on failure

mod codec;
mod protocol;

pub use codec::{Payment, PaymentAck, PaymentAckCodec, PaymentCodec, PseudosettleCodecError};
pub use protocol::{
    PseudosettleInboundProtocol, PseudosettleInboundResult, PseudosettleOutboundProtocol,
    PseudosettleResponder, inbound, outbound, validate_timestamp,
};

#[allow(unreachable_pub)]
mod proto {
    include!(concat!(env!("OUT_DIR"), "/proto/mod.rs"));
}

/// Protocol name for pseudosettle.
pub const PROTOCOL_NAME: &str = "/swarm/pseudosettle/1.0.0/pseudosettle";
