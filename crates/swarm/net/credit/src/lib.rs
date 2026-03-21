//! Credit protocol for Swarm credit limit announcement.

mod codec;
pub use codec::AnnounceCreditLimit;

mod error;
pub use error::CreditError;

mod protocol;
pub use protocol::{CreditInboundProtocol, CreditOutboundProtocol, inbound, outbound};

/// Protocol name for credit limit announcement.
// NOTE: wire-compat — must remain "/swarm/pricing/1.0.0/pricing" for interoperability.
pub const PROTOCOL_NAME: &str = "/swarm/pricing/1.0.0/pricing";
