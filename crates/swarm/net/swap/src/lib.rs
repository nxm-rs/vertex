//! SWAP protocol for Swarm cheque-based bandwidth settlement.
//!
//! The signed cheque travels as a JSON object embedded in a protobuf `bytes`
//! field. The JSON shape is fixed and byte-identical to the live network for
//! interoperability; the encode/decode lives in
//! [`vertex_swarm_bandwidth_chequebook::SignedCheque::to_json`] and
//! [`from_json`](vertex_swarm_bandwidth_chequebook::SignedCheque::from_json),
//! never in this crate.

mod codec;
mod error;
mod headers;
mod protocol;

pub use codec::{EmitCheque, EmitChequeCodec, Handshake, HandshakeCodec};
pub use error::SwapError;
pub use headers::{HEADER_DEDUCTION, HEADER_EXCHANGE_RATE, SettlementHeaders};
pub use protocol::{SwapInboundProtocol, SwapOutboundProtocol, inbound, outbound};

// Re-export SignedCheque for convenience
pub use vertex_swarm_bandwidth_chequebook::SignedCheque;

/// Protocol name for SWAP.
pub const PROTOCOL_NAME: &str = "/swarm/swap/1.0.0/swap";
