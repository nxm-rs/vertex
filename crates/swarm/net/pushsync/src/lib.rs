//! Pushsync protocol for Swarm chunk push and storage receipt.

mod codec;
pub use codec::{Delivery, Receipt, ReceiptResponse};

mod error;
pub use error::PushsyncError;

mod receipt;
pub use receipt::SignedReceipt;

mod protocol;
pub use protocol::{
    PushsyncInboundProtocol, PushsyncOutboundProtocol, PushsyncResponder, inbound, outbound,
};

/// Protocol name for pushsync.
pub const PROTOCOL_NAME: &str = "/swarm/pushsync/1.3.1/pushsync";
