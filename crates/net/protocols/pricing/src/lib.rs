//! Pricing protocol for Swarm bandwidth accounting.
//!
//! This crate provides the wire protocol types for exchanging payment thresholds
//! between peers. It is **pure protocol plumbing** - it does NOT make business
//! decisions about thresholds.
//!
//! # Protocol
//!
//! - Path: `/swarm/pricing/1.0.0/pricing`
//! - Symmetric: Both peers announce on connection
//! - Message: `AnnouncePaymentThreshold` with payment threshold as bytes (big-endian)
//!
//! # Usage
//!
//! This crate is used by `vertex-net-client`'s `SwarmClientHandler` which handles
//! the pricing protocol as part of its multi-protocol connection handler.
//!
//! # Business Logic (NOT in this crate)
//!
//! - What threshold to announce (based on peer type, config)
//! - Whether to validate received thresholds
//! - What minimum threshold to accept
//! - What action to take on invalid thresholds

mod codec;
mod protocol;

pub use codec::{AnnouncePaymentThreshold, PricingCodecError};
pub use protocol::{inbound, outbound, PricingInboundProtocol, PricingOutboundProtocol};

mod proto {
    include!(concat!(env!("OUT_DIR"), "/proto/mod.rs"));
}

/// Protocol name for pricing.
pub const PROTOCOL_NAME: &str = "/swarm/pricing/1.0.0/pricing";
