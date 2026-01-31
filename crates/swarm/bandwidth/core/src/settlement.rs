//! Settlement provider trait for bandwidth accounting.
//!
//! Settlement providers implement specific settlement mechanisms (pseudosettle, swap)
//! that can be composed to support different [`BandwidthMode`](vertex_swarm_api::BandwidthMode)
//! configurations.
//!
//! # Provider Lifecycle
//!
//! Providers are called at two points:
//!
//! 1. **`pre_allow()`**: Before checking if a transfer is allowed. Providers can adjust
//!    the peer's balance here (e.g., pseudosettle adds time-based credit).
//!
//! 2. **`settle()`**: When explicit settlement is requested (balance exceeds threshold).
//!    Providers can issue payments here (e.g., swap sends a cheque).
//!
//! # Composition
//!
//! Multiple providers can be composed using [`ProviderList`](crate::ProviderList).
//! For `BandwidthMode::Both`, pseudosettle runs first to refresh allowance,
//! then swap settles any remaining debt.

use crate::accounting::{AccountingError, PeerState};

/// A settlement provider that can adjust peer balances.
///
/// Settlement providers are called during accounting operations to implement
/// specific settlement mechanisms. Each provider handles one aspect of settlement:
///
/// - **Pseudosettle**: Time-based debt forgiveness in `pre_allow()`
/// - **Swap**: Chequebook payments in `settle()`
#[async_trait::async_trait]
pub trait SettlementProvider: Send + Sync + 'static {
    /// Called before checking if a transfer is allowed.
    ///
    /// The provider can adjust the peer's balance here. For example, pseudosettle
    /// adds time-based credit to reduce outstanding debt.
    ///
    /// Returns the amount of debt that was settled (positive = debt reduced).
    fn pre_allow(&self, state: &PeerState) -> i64;

    /// Called when explicit settlement is requested.
    ///
    /// This is called when the peer's balance exceeds the payment threshold
    /// and settlement is needed. For example, swap issues a cheque here.
    ///
    /// Returns the amount settled (positive = debt reduced), or an error if settlement failed.
    async fn settle(&self, state: &PeerState) -> Result<i64, AccountingError>;

    /// Human-readable name for logging and debugging.
    fn name(&self) -> &'static str;
}

/// A no-op settlement provider that does nothing.
///
/// Use this when no settlement mechanism is configured (`BandwidthMode::None`).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoSettlement;

#[async_trait::async_trait]
impl SettlementProvider for NoSettlement {
    fn pre_allow(&self, _state: &PeerState) -> i64 {
        0
    }

    async fn settle(&self, _state: &PeerState) -> Result<i64, AccountingError> {
        Ok(0)
    }

    fn name(&self) -> &'static str {
        "none"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vertex_swarm_primitives::OverlayAddress;

    fn test_peer_state() -> PeerState {
        PeerState::new(OverlayAddress::from([1u8; 32]), 13_500_000, 16_875_000)
    }

    #[test]
    fn test_no_settlement_provider() {
        let provider = NoSettlement;
        let state = test_peer_state();

        assert_eq!(provider.pre_allow(&state), 0);
        assert_eq!(provider.name(), "none");
    }
}
