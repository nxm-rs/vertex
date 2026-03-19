compile_error!("vertex-swarm-net-swap is disabled: depends on serde_json which has been removed from the workspace. Remove serde_json dependency before re-enabling.");

//! SWAP protocol for Swarm cheque-based bandwidth settlement.

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
