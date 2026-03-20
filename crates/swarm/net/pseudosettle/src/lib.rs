//! Pseudosettle protocol for Swarm bandwidth accounting with micro-payments.

mod codec;
pub use codec::{Payment, PaymentAck};

mod error;
pub use error::PseudosettleError;

mod protocol;
pub use protocol::{PseudosettleInboundResult, PseudosettleResponder, inbound, outbound};

/// Protocol name for pseudosettle.
pub const PROTOCOL_NAME: &str = "/swarm/pseudosettle/1.0.0/pseudosettle";
