//! Pricing protocol for Swarm payment threshold announcement.

mod codec;
pub use codec::AnnouncePaymentThreshold;

mod error;
pub use error::PricingError;

mod protocol;
pub use protocol::{PricingInboundProtocol, PricingOutboundProtocol, inbound, outbound};

/// Protocol name for pricing.
pub const PROTOCOL_NAME: &str = "/swarm/pricing/1.0.0/pricing";
