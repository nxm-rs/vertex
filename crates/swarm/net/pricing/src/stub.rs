//! Stub-mode observer for the pricing protocol.
//!
//! Used by node types that advertise the protocol for interop but do not
//! participate in chunk accounting (bootnodes today). Inbound thresholds are
//! observed at `debug` and discarded.

use alloy_primitives::U256;
use libp2p::PeerId;
use tracing::debug;

use crate::AnnouncePaymentThreshold;

/// Observer notified when a peer announces its payment threshold.
///
/// The trait is used to decouple the pricing behaviour from concrete accounting
/// implementations. The stub variant ([`StubObserver`]) discards updates; a full
/// implementation (e.g. a colocated client node) can implement this trait to
/// receive thresholds into the accounting subsystem.
#[auto_impl::auto_impl(&, Arc, Box)]
pub trait PaymentThresholdObserver: Send + Sync {
    /// Record an announced payment threshold from a remote peer.
    fn record_threshold(&self, peer: PeerId, threshold: U256);
}

/// No-op observer used in bootnode stub mode.
#[derive(Debug, Default, Clone, Copy)]
pub struct StubObserver;

impl PaymentThresholdObserver for StubObserver {
    fn record_threshold(&self, peer: PeerId, threshold: U256) {
        debug!(
            %peer,
            %threshold,
            "Pricing stub: discarding peer threshold (bootnode does not account)"
        );
    }
}

/// Helper for constructing a stub-mode announcement payload from a u64 default.
pub(crate) fn stub_announcement(default_threshold: u64) -> AnnouncePaymentThreshold {
    AnnouncePaymentThreshold::new(U256::from(default_threshold))
}
