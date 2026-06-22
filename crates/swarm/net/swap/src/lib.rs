//! SWAP protocol for Swarm cheque-based bandwidth settlement.
//!
//! The signed cheque travels as a JSON object embedded in a protobuf `bytes`
//! field, encoded and decoded with `serde_json` over
//! [`vertex_swarm_accounting_chequebook::SignedCheque`]. The JSON is
//! transport-only: the cheque signature is EIP-712 over the cheque fields, not
//! over the JSON bytes.

mod codec;
mod error;
mod headers;
mod protocol;

pub use codec::{EmitCheque, EmitChequeCodec, Handshake, HandshakeCodec};
pub use error::SwapError;
pub use headers::{HEADER_DEDUCTION, HEADER_EXCHANGE_RATE, SettlementHeaders};
pub use protocol::{SwapInboundProtocol, SwapOutboundProtocol, inbound, outbound};

// Re-export SignedCheque for convenience
pub use vertex_swarm_accounting_chequebook::SignedCheque;

/// Protocol name for SWAP.
pub const PROTOCOL_NAME: &str = "/swarm/swap/1.0.0/swap";
