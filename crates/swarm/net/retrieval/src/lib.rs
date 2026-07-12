//! Retrieval protocol for Swarm chunk request and delivery.

#[cfg(any(test, feature = "arbitrary"))]
mod arbitrary_impls;

mod codec;
pub use codec::{Delivery, Request};

mod error;
pub use error::RetrievalError;

#[cfg(any(test, feature = "arbitrary"))]
pub mod fuzz;

mod protocol;
pub use protocol::{
    RetrievalInboundProtocol, RetrievalOutboundProtocol, RetrievalResponder, inbound, outbound,
};

/// Protocol name for retrieval.
pub const PROTOCOL_NAME: &str = "/swarm/retrieval/1.4.0/retrieval";
