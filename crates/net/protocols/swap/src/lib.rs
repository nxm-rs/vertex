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

#[allow(unreachable_pub)]
mod proto {
    include!(concat!(env!("OUT_DIR"), "/proto/mod.rs"));
}

/// Protocol name for SWAP.
pub const PROTOCOL_NAME: &str = "/swarm/swap/1.0.0/swap";
