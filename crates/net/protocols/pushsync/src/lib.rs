//! Pushsync protocol for Swarm chunk push/receipt.
//!
//! This crate implements the wire protocol for pushing chunks to peers for storage.
//! It is compatible with Bee's `/swarm/pushsync/1.3.1/pushsync` protocol.
//!
//! # Protocol Flow
//!
//! Pushsync is a request/response protocol:
//! - **Pusher (outbound)**: Send `Delivery` with chunk, receive `Receipt`
//! - **Storer (inbound)**: Receive `Delivery`, send `Receipt` after storing
//!
//! # Example
//!
//! ```ignore
//! // Outbound: Push a chunk
//! let delivery = Delivery::new(address, data, stamp);
//! let protocol = pushsync::outbound(delivery);
//! let receipt = upgrade_outbound(stream, protocol).await?;
//!
//! // Inbound: Store a chunk
//! let protocol = pushsync::inbound();
//! let (delivery, responder) = upgrade_inbound(stream, protocol).await?;
//! // Store the chunk...
//! responder.send_receipt(receipt).await?;
//! ```

mod codec;
mod protocol;

// Include generated protobuf code
mod proto {
    include!(concat!(env!("OUT_DIR"), "/proto/mod.rs"));
}

pub use codec::{Delivery, DeliveryCodec, PushsyncCodecError, Receipt, ReceiptCodec};
pub use protocol::{
    inbound, outbound, PushsyncInboundProtocol, PushsyncOutboundProtocol, PushsyncResponder,
};

/// Protocol name for pushsync.
pub const PROTOCOL_NAME: &str = "/swarm/pushsync/1.3.1/pushsync";
