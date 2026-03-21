//! Time-based settlement provider for bandwidth accounting.
//!
//! Peers accumulate a time-based allowance (refresh rate x elapsed seconds)
//! that forgives debt periodically, enabling bandwidth usage without payments.
//!
//! # Usage
//!
//! Use [`create_pseudosettle_actor`] to create a service/handle pair.
//! The [`PseudosettleProvider`] implements [`SwarmSettlementProvider`] for
//! integration with [`Accounting`].
//!
//! [`SwarmSettlementProvider`]: vertex_swarm_api::SwarmSettlementProvider

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

mod error;
mod handle;
mod service;

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc;
use vertex_swarm_api::{
    BandwidthMode, SwarmAccountingConfig, SwarmError, SwarmPeerAccounting, SwarmPeerBandwidth,
    SwarmPeerRegistry, SwarmResult, SwarmSettlementProvider,
};
use vertex_swarm_node::ClientCommand;
use vertex_swarm_primitives::OverlayAddress;

pub use error::PseudosettleSettlementError;
pub use handle::PseudosettleHandle;
pub use service::{PseudosettleCommand, PseudosettleService};
pub use vertex_swarm_node::PseudosettleEvent;

/// Time-based debt forgiveness provider.
///
/// On `pre_allow()`, credits the peer based on elapsed time:
/// `credit = min(elapsed_seconds x refresh_rate, abs(debt))`
///
/// With a handle, `settle()` delegates to the network service.
/// Without a handle, only local refresh via `pre_allow()` is performed.
pub struct PseudosettleProvider<C> {
    config: C,
    /// Optional handle for delegating to the service.
    handle: Option<PseudosettleHandle>,
}

impl<C: SwarmAccountingConfig> PseudosettleProvider<C> {
    /// Create a pseudosettle provider with a handle for network settlement.
    ///
    /// Settlement requires a network round-trip: the debtor sends a
    /// `Payment` message and the creditor replies with a `PaymentAck`
    /// containing the accepted amount. Use [`create_pseudosettle_actor`]
    /// to obtain the handle.
    pub fn new(config: C, handle: PseudosettleHandle) -> Self {
        Self {
            config,
            handle: Some(handle),
        }
    }

    /// Create a provider for testing without a network service.
    ///
    /// `settle()` will return an error; only `pre_allow()` (time-based
    /// refresh) is functional.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn new_without_handle(config: C) -> Self {
        Self {
            config,
            handle: None,
        }
    }

    /// Get the refresh rate from the configuration.
    pub fn refresh_rate(&self) -> u64 {
        self.config.refresh_rate()
    }

    /// Get a reference to the configuration.
    pub fn config(&self) -> &C {
        &self.config
    }
}

#[async_trait::async_trait]
impl<C: SwarmAccountingConfig + 'static> SwarmSettlementProvider for PseudosettleProvider<C> {
    fn supported_mode(&self) -> BandwidthMode {
        BandwidthMode::Pseudosettle
    }

    fn pre_allow(&self, _peer: OverlayAddress, state: &dyn SwarmPeerAccounting) -> i64 {
        state.apply_refresh(current_timestamp(), self.config.refresh_rate())
    }

    async fn settle(
        &self,
        peer: OverlayAddress,
        state: &dyn SwarmPeerAccounting,
    ) -> SwarmResult<i64> {
        // If we have a handle, delegate to the service
        if let Some(handle) = &self.handle {
            let balance = state.balance();
            if balance >= 0 {
                return Ok(0i64); // Nothing to settle
            }

            let amount = (-balance) as u64;
            let accepted = handle
                .settle(peer, amount)
                .await
                .map_err(SwarmError::payment_required)?;

            Ok(accepted as i64)
        } else {
            // No network service -- cannot settle without a handle.
            Err(SwarmError::payment_required(
                PseudosettleSettlementError::ServiceStopped,
            ))
        }
    }

    fn name(&self) -> &'static str {
        "pseudosettle"
    }
}

/// Create a pseudosettle actor (service + handle pair).
///
/// Spawn the service as a background task. Use the handle to create
/// a [`PseudosettleProvider`].
pub fn create_pseudosettle_actor<A: SwarmPeerRegistry<Peer: SwarmPeerBandwidth> + 'static>(
    event_rx: mpsc::UnboundedReceiver<PseudosettleEvent>,
    client_command_tx: mpsc::UnboundedSender<ClientCommand>,
    accounting: Arc<A>,
    refresh_rate: u64,
) -> (PseudosettleService<A>, PseudosettleHandle) {
    let (command_tx, command_rx) = mpsc::unbounded_channel();

    let service = PseudosettleService::new(
        command_rx,
        event_rx,
        client_command_tx,
        accounting,
        refresh_rate,
    );

    let handle = PseudosettleHandle::new(command_tx);

    (service, handle)
}

/// Get current timestamp in seconds.
fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Create a pseudosettle-only accounting instance for testing.
///
/// The provider has no network handle, so `settle()` will error.
/// Only `pre_allow()` (time-based refresh) is functional.
#[cfg(any(test, feature = "test-utils"))]
pub fn new_pseudosettle_accounting<
    C: SwarmAccountingConfig + Clone + 'static,
    I: vertex_swarm_api::SwarmIdentity,
>(
    config: C,
    identity: I,
) -> vertex_swarm_bandwidth::Accounting<C, I> {
    vertex_swarm_bandwidth::Accounting::with_providers(
        config.clone(),
        identity,
        vec![Box::new(PseudosettleProvider::new_without_handle(config))],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use vertex_swarm_api::{Direction, SwarmPeerBandwidth, SwarmPeerRegistry};
    use vertex_swarm_bandwidth::BandwidthConfig;
    use vertex_swarm_bandwidth::PeerAccounting;
    use vertex_swarm_test_utils::{test_identity, test_peer};

    // Fixed base time for deterministic tests.
    const T0: u64 = 1_000_000;

    #[test]
    fn test_pseudosettle_provider_name() {
        let provider = PseudosettleProvider::new_without_handle(BandwidthConfig::default());
        assert_eq!(provider.name(), "pseudosettle");
    }

    #[test]
    fn test_pseudosettle_refresh_rate() {
        let provider = PseudosettleProvider::new_without_handle(BandwidthConfig::default());
        assert_eq!(provider.refresh_rate(), 4_500_000);
    }

    #[test]
    fn test_pseudosettle_accounting_basic() {
        let accounting = new_pseudosettle_accounting(BandwidthConfig::default(), test_identity());

        let handle = accounting.for_peer(test_peer());
        assert_eq!(handle.balance(), 0);

        handle.record(1000, Direction::Upload);
        assert_eq!(handle.balance(), 1000);

        handle.record(500, Direction::Download);
        assert_eq!(handle.balance(), 500);
    }

    #[test]
    fn test_refresh_positive_balance_no_credit() {
        let state = PeerAccounting::new(13_500_000, 16_875_000);
        state.add_balance(1000);

        let credit = state.apply_refresh(T0, 4_500_000);

        assert_eq!(credit, 0);
        assert_eq!(state.balance(), 1000);
    }

    #[test]
    fn test_refresh_negative_balance() {
        let state = PeerAccounting::new(13_500_000, 16_875_000);
        state.add_balance(-1000);
        state.set_last_refresh(T0);

        let credit = state.apply_refresh(T0 + 1, 100);

        assert_eq!(credit, 100);
        assert_eq!(state.balance(), -900);
    }

    #[test]
    fn test_refresh_caps_at_zero() {
        let state = PeerAccounting::new(13_500_000, 16_875_000);
        state.add_balance(-100);
        state.set_last_refresh(T0);

        let credit = state.apply_refresh(T0 + 10, 1000);

        assert_eq!(credit, 100);
        assert_eq!(state.balance(), 0);
    }

    #[test]
    fn test_refresh_same_second_no_credit() {
        let state = PeerAccounting::new(13_500_000, 16_875_000);
        state.set_last_refresh(T0);
        state.add_balance(-10_000);

        let credit = state.apply_refresh(T0, 4_500_000);

        assert_eq!(credit, 0);
        assert_eq!(state.balance(), -10_000);
    }

    #[test]
    fn test_refresh_time_limited_partial() {
        let state = PeerAccounting::new(13_500_000, 16_875_000);
        state.add_balance(-50_000);
        state.set_last_refresh(T0);

        let credit = state.apply_refresh(T0 + 3, 10_000);

        assert_eq!(credit, 30_000);
        assert_eq!(state.balance(), -20_000);
    }

    #[test]
    fn test_refresh_full_debt_acceptance() {
        let state = PeerAccounting::new(13_500_000, 16_875_000);
        state.add_balance(-10_000);
        state.set_last_refresh(T0);

        let credit = state.apply_refresh(T0 + 10, 10_000);

        assert_eq!(credit, 10_000);
        assert_eq!(state.balance(), 0);
    }

    #[test]
    fn test_refresh_sequential() {
        let state = PeerAccounting::new(13_500_000, 16_875_000);
        state.add_balance(-100_000);
        state.set_last_refresh(T0);

        let credit1 = state.apply_refresh(T0 + 5, 10_000);
        assert_eq!(credit1, 50_000);
        assert_eq!(state.balance(), -50_000);

        // Same second -- no credit
        let credit2 = state.apply_refresh(T0 + 5, 10_000);
        assert_eq!(credit2, 0);
        assert_eq!(state.balance(), -50_000);

        // 3 more seconds
        let credit3 = state.apply_refresh(T0 + 8, 10_000);
        assert_eq!(credit3, 30_000);
        assert_eq!(state.balance(), -20_000);
    }

    #[test]
    fn test_refresh_first_call_initialises() {
        let state = PeerAccounting::new(13_500_000, 16_875_000);

        assert_eq!(state.last_refresh(), 0);
        state.add_balance(-10_000);

        let credit = state.apply_refresh(T0, 10_000);

        assert_eq!(credit, 0);
        assert_eq!(state.balance(), -10_000);
        assert_eq!(state.last_refresh(), T0);
    }

    #[test]
    fn test_refresh_client_vs_storer_rate() {
        let storer_state = PeerAccounting::new(13_500_000, 16_875_000);
        let client_state = PeerAccounting::new_client_only(13_500_000, 16_875_000, 10);

        storer_state.add_balance(-50_000);
        client_state.add_balance(-50_000);
        storer_state.set_last_refresh(T0);
        client_state.set_last_refresh(T0);

        let storer_credit = storer_state.apply_refresh(T0 + 10, 10_000);
        assert_eq!(storer_credit, 50_000);
        assert_eq!(storer_state.balance(), 0);

        let client_credit = client_state.apply_refresh(T0 + 10, 1_000);
        assert_eq!(client_credit, 10_000);
        assert_eq!(client_state.balance(), -40_000);
    }
}
