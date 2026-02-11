//! Pseudosettle protocol for Swarm bandwidth accounting with micro-payments.

mod codec;
mod error;
mod protocol;

pub use codec::{Payment, PaymentAck, PaymentAckCodec, PaymentCodec};
pub use error::PseudosettleError;
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
