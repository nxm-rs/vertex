//! Pricing protocol for Swarm payment threshold announcement.
//!
//! The wire name is historical: the only payload is a payment threshold, a
//! credit line a peer extends, not a chunk price.

#[cfg(any(test, feature = "arbitrary"))]
mod arbitrary_impls;

mod codec;
pub use codec::AnnouncePaymentThreshold;

mod error;
pub use error::PricingError;

#[cfg(any(test, feature = "arbitrary"))]
pub mod fuzz;

mod protocol;
pub use protocol::{PricingInboundProtocol, PricingOutboundProtocol, inbound, outbound};

/// Protocol name for pricing.
pub const PROTOCOL_NAME: &str = "/swarm/pricing/1.0.0/pricing";
