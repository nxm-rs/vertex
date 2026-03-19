//! Retrieval protocol for Swarm chunk request and delivery.

mod codec;
mod error;
mod protocol;

pub use codec::{Delivery, DeliveryCodec, Request, RequestCodec};
pub use error::RetrievalError;
pub use protocol::{
    RetrievalInboundProtocol, RetrievalOutboundProtocol, RetrievalResponder, inbound, outbound,
};

/// Protocol name for retrieval.
pub const PROTOCOL_NAME: &str = "/swarm/retrieval/1.4.0/retrieval";
