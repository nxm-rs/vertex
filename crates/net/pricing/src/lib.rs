//! Pricing protocol for Swarm bandwidth accounting.
//!
//! The pricing protocol allows peers to announce their payment thresholds
//! to each other. This is used by the availability accounting system to
//! know when to request/expect settlement.
//!
//! # Protocol
//!
//! - Path: `/swarm/pricing/1.0.0/pricing`
//! - Symmetric: Both peers announce on connection
//! - Message: `AnnouncePaymentThreshold` with payment threshold as bytes (big-endian)
//!
//! # Flow
//!
//! When a connection is established:
//! 1. Both peers open a pricing stream to each other
//! 2. Each sends their `AnnouncePaymentThreshold`
//! 3. The accounting system is notified of the peer's threshold
//!
//! Light nodes receive a lower threshold than full nodes.

mod behaviour;
mod codec;
mod handler;
mod protocol;

pub use behaviour::{PricingBehaviour, PricingConfig, PricingEvent};
pub use codec::{AnnouncePaymentThreshold, PricingCodecError};
pub use handler::{Command as HandlerCommand, Config as HandlerConfig, Event as HandlerEvent, Handler};
pub use protocol::{PricingError, PricingInboundOutput, PricingOutboundOutput, PricingProtocol};

mod proto {
    include!(concat!(env!("OUT_DIR"), "/proto/mod.rs"));
}

use alloy_primitives::U256;

/// Protocol name for pricing.
pub const PROTOCOL_NAME: &str = "/swarm/pricing/1.0.0/pricing";

/// Default payment threshold for full nodes (13,500,000 AU - matches Bee).
pub const DEFAULT_PAYMENT_THRESHOLD: u64 = 13_500_000;

/// Default payment threshold for light nodes (1,350,000 AU - 1/10th of full).
pub const DEFAULT_LIGHT_PAYMENT_THRESHOLD: u64 = 1_350_000;

/// Minimum acceptable payment threshold.
///
/// Peers announcing a threshold below this are disconnected.
pub const MIN_PAYMENT_THRESHOLD: u64 = 1_000;

/// Observer trait for payment threshold notifications.
///
/// Implement this trait to receive notifications when a peer announces
/// their payment threshold. The accounting system implements this to
/// update per-peer thresholds.
pub trait PaymentThresholdObserver: Send + Sync {
    /// Called when a peer announces their payment threshold.
    ///
    /// # Arguments
    /// * `peer` - The peer's overlay address (not PeerId)
    /// * `threshold` - The announced payment threshold
    fn on_payment_threshold(&self, peer: &[u8; 32], threshold: U256);
}

/// No-op observer for testing or when accounting is disabled.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoOpObserver;

impl PaymentThresholdObserver for NoOpObserver {
    fn on_payment_threshold(&self, _peer: &[u8; 32], _threshold: U256) {}
}
