//! Pricing protocol for Swarm payment threshold announcement.

mod codec;
mod error;
mod protocol;

pub use codec::{AnnouncePaymentThreshold, PricingCodec};
pub use error::PricingError;
pub use protocol::{PricingInboundProtocol, PricingOutboundProtocol, inbound, outbound};

/// Protocol name for pricing.
pub const PROTOCOL_NAME: &str = "/swarm/pricing/1.0.0/pricing";
