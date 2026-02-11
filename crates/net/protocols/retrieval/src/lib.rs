//! Retrieval protocol for Swarm chunk request and delivery.

mod codec;
mod error;
mod protocol;

// Include generated protobuf code
#[allow(unreachable_pub)]
mod proto {
    include!(concat!(env!("OUT_DIR"), "/proto/mod.rs"));
}

pub use codec::{Delivery, DeliveryCodec, Request, RequestCodec};
pub use error::RetrievalError;
pub use protocol::{
    RetrievalInboundProtocol, RetrievalOutboundProtocol, RetrievalResponder, inbound, outbound,
};

/// Protocol name for retrieval.
pub const PROTOCOL_NAME: &str = "/swarm/retrieval/1.4.0/retrieval";
