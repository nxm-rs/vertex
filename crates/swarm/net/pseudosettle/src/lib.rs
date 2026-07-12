//! Pseudosettle protocol for Swarm bandwidth accounting with micro-payments.

#[cfg(any(test, feature = "arbitrary"))]
mod arbitrary_impls;

mod codec;
pub use codec::{Payment, PaymentAck};

mod error;
pub use error::PseudosettleError;

#[cfg(any(test, feature = "arbitrary"))]
pub mod fuzz;

mod protocol;
pub use protocol::{PseudosettleInboundResult, PseudosettleResponder, inbound, outbound};

/// Protocol name for pseudosettle.
pub const PROTOCOL_NAME: &str = "/swarm/pseudosettle/1.0.0/pseudosettle";
