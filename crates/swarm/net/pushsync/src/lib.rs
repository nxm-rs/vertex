//! Pushsync protocol for Swarm chunk push and storage receipt.

mod codec;
mod error;
mod protocol;

pub use codec::{Delivery, DeliveryCodec, Receipt, ReceiptCodec};
pub use error::PushsyncError;
pub use protocol::{
    PushsyncInboundProtocol, PushsyncOutboundProtocol, PushsyncResponder, inbound, outbound,
};

/// Protocol name for pushsync.
pub const PROTOCOL_NAME: &str = "/swarm/pushsync/1.3.1/pushsync";
