//! SWAP protocol for Swarm bandwidth accounting with cheque settlement.
//!
//! This crate provides the wire protocol for exchanging signed cheques between
//! peers. It is **pure protocol plumbing** - it does NOT make business decisions
//! about settlements or cheque validation.
//!
//! # Protocol
//!
//! - Path: `/swarm/swap/1.0.0/swap`
//! - Pattern: Request with header negotiation (headler pattern)
//! - Headers: Exchange rate negotiated via `exchange` and `deduction` headers
//! - Request: `EmitCheque { cheque }` - JSON-encoded `SignedCheque`
//!
//! # Exchange Rate Negotiation
//!
//! The "headler" pattern is used to negotiate exchange rates:
//!
//! 1. Initiator sends their exchange rate in request headers
//! 2. Responder sends their exchange rate in response headers
//! 3. Initiator then sends the cheque
//!
//! # Usage
//!
//! ## Sending a cheque (outbound)
//!
//! ```ignore
//! use vertex_net_swap::{outbound, SettlementHeaders};
//! use vertex_swarm_bandwidth_chequebook::SignedCheque;
//!
//! let cheque: SignedCheque = /* ... */;
//! let our_rate = U256::from(1_000_000u64);
//! let protocol = outbound(cheque, our_rate);
//!
//! // After negotiation...
//! let peer_headers: SettlementHeaders = protocol.upgrade_outbound(...).await?;
//! ```
//!
//! ## Receiving a cheque (inbound)
//!
//! ```ignore
//! use vertex_net_swap::{inbound, SettlementHeaders};
//!
//! let our_rate = U256::from(1_000_000u64);
//! let protocol = inbound(our_rate);
//!
//! // After negotiation...
//! let (cheque, peer_headers): (SignedCheque, SettlementHeaders) =
//!     protocol.upgrade_inbound(...).await?;
//! ```
//!
//! # Business Logic (NOT in this crate)
//!
//! - Cheque validation (signature, amount, cumulative payout)
//! - Balance updates
//! - Exchange rate determination
//! - On-chain cashing decisions

mod codec;
mod headers;
mod protocol;

pub use codec::{EmitCheque, EmitChequeCodec, Handshake, HandshakeCodec, SwapCodecError};
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
