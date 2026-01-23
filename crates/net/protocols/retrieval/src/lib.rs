//! Retrieval protocol for Swarm chunk request/response.
//!
//! This crate implements the wire protocol for retrieving chunks from peers.
//! It is compatible with Bee's `/swarm/retrieval/1.4.0/retrieval` protocol.
//!
//! # Protocol Flow
//!
//! Retrieval is a simple request/response protocol:
//! - **Requester (outbound)**: Send `Request` with chunk address, receive `Delivery`
//! - **Responder (inbound)**: Receive `Request`, send `Delivery` with chunk data or error
//!
//! # Example
//!
//! ```ignore
//! // Outbound: Request a chunk
//! let protocol = retrieval::outbound(chunk_address);
//! let delivery = upgrade_outbound(stream, protocol).await?;
//!
//! // Inbound: Respond to a chunk request
//! let protocol = retrieval::inbound();
//! let (request, responder) = upgrade_inbound(stream, protocol).await?;
//! responder.send_chunk(data, stamp).await?;
//! ```

mod codec;
mod protocol;

// Include generated protobuf code
mod proto {
    include!(concat!(env!("OUT_DIR"), "/proto/mod.rs"));
}

pub use codec::{Delivery, DeliveryCodec, Request, RequestCodec, RetrievalCodecError};
pub use protocol::{
    RetrievalInboundProtocol, RetrievalOutboundProtocol, RetrievalResponder, inbound, outbound,
};

/// Protocol name for retrieval.
pub const PROTOCOL_NAME: &str = "/swarm/retrieval/1.4.0/retrieval";
