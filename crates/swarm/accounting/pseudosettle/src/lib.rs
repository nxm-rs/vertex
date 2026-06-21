//! Time-based settlement provider for bandwidth accounting.
//!
//! Peers accumulate a time-based allowance (refresh rate × elapsed seconds)
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

use tokio::sync::mpsc;
use vertex_swarm_accounting::Accounting;
use vertex_swarm_api::{
    Au, BandwidthMode, SwarmAccountingConfig, SwarmBandwidthAccounting, SwarmError, SwarmIdentity,
    SwarmPeerState, SwarmResult, SwarmSettlementProvider,
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
/// `credit = min(elapsed_seconds × refresh_rate, abs(debt))`
///
/// With a handle, `settle()` delegates to the network service.
/// Without a handle, only local refresh via `pre_allow()` is performed.
pub struct PseudosettleProvider<C> {
    config: C,
    /// Optional handle for delegating to the service.
    handle: Option<PseudosettleHandle>,
}

impl<C: SwarmAccountingConfig> PseudosettleProvider<C> {
    /// Create a new pseudosettle provider with the given configuration.
    ///
    /// This creates a provider without network settlement capability.
    /// Use [`create_pseudosettle_actor`] for full functionality.
    pub fn new(config: C) -> Self {
        Self {
            config,
            handle: None,
        }
    }

    /// Create a new pseudosettle provider with a handle for network settlement.
    pub fn with_handle(config: C, handle: PseudosettleHandle) -> Self {
        Self {
            config,
            handle: Some(handle),
        }
    }

    /// Get the refresh rate in AU per second from the configuration.
    pub fn refresh_rate(&self) -> Au {
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

    fn pre_allow(&self, _peer: OverlayAddress, state: &dyn SwarmPeerState) -> Au {
        refresh_allowance(state, self.config.refresh_rate())
    }

    async fn settle(&self, peer: OverlayAddress, state: &dyn SwarmPeerState) -> SwarmResult<Au> {
        // If we have a handle, delegate to the service
        if let Some(handle) = &self.handle {
            let balance = state.balance();
            if !balance.is_negative() {
                return Ok(Au::ZERO); // Nothing to settle
            }

            let amount = balance.unsigned_abs();
            let accepted = handle
                .settle(peer, amount)
                .await
                .map_err(SwarmError::payment_required)?;

            Ok(accepted)
        } else {
            // No handle - refresh happens automatically in pre_allow()
            Ok(Au::ZERO)
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
pub fn create_pseudosettle_actor<A: SwarmBandwidthAccounting + 'static>(
    event_rx: mpsc::UnboundedReceiver<PseudosettleEvent>,
    client_command_tx: mpsc::UnboundedSender<ClientCommand>,
    accounting: Arc<A>,
    refresh_rate: Au,
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

/// Apply time-based credit to peer's negative balance. Returns credit applied.
fn refresh_allowance(state: &dyn SwarmPeerState, refresh_rate: Au) -> Au {
    let now = current_timestamp();
    let last = state.last_refresh();

    if last == 0 {
        state.set_last_refresh(now);
        return Au::ZERO;
    }

    let elapsed = now.saturating_sub(last);
    if elapsed == 0 {
        return Au::ZERO;
    }

    // Allowance is refresh_rate AU per elapsed second. The scaling is checked
    // so a large rate times a long gap cannot wrap into a tiny allowance; on
    // overflow the allowance saturates at the maximum, which is then capped by
    // the actual debt below.
    let allowance = refresh_rate
        .checked_scale(elapsed)
        .unwrap_or(Au::from_amount(u64::MAX));

    // Only add credit if balance is negative (we owe them)
    let balance = state.balance();
    let credit = if balance.is_negative() {
        let credit = allowance.min(-balance);
        state.add_balance(credit);
        credit
    } else {
        Au::ZERO
    };

    state.set_last_refresh(now);
    credit
}

/// Get current timestamp in seconds.
fn current_timestamp() -> u64 {
    vertex_util_runtime::time::now_unix_secs()
}

/// Create a new pseudosettle-only accounting instance.
///
/// This is a convenience function that creates a `Accounting` with
/// a `PseudosettleProvider`.
pub fn new_pseudosettle_accounting<C: SwarmAccountingConfig + Clone + 'static, I: SwarmIdentity>(
    config: C,
    identity: I,
) -> Accounting<C, I> {
    Accounting::with_providers(
        config.clone(),
        identity,
        vec![Box::new(PseudosettleProvider::new(config))],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use vertex_swarm_accounting::BandwidthConfig;
    use vertex_swarm_accounting::PeerState;
    use vertex_swarm_api::{Direction, SwarmBandwidthAccounting, SwarmPeerBandwidth};
    use vertex_swarm_test_utils::{test_identity, test_peer};

    #[test]
    fn test_pseudosettle_provider_name() {
        let provider = PseudosettleProvider::new(BandwidthConfig::default());
        assert_eq!(provider.name(), "pseudosettle");
    }

    #[test]
    fn test_pseudosettle_refresh_rate() {
        let provider = PseudosettleProvider::new(BandwidthConfig::default());
        assert_eq!(provider.refresh_rate(), Au::from_amount(4_500_000));
    }

    #[test]
    fn test_pseudosettle_accounting_basic() {
        let accounting = new_pseudosettle_accounting(BandwidthConfig::default(), test_identity());

        let handle = accounting.for_peer(test_peer());
        assert_eq!(handle.balance(), Au::from_amount(0));

        handle.record(Au::from_amount(1000), Direction::Upload);
        assert_eq!(handle.balance(), Au::from_amount(1000));

        handle.record(Au::from_amount(500), Direction::Download);
        assert_eq!(handle.balance(), Au::from_amount(500));
    }

    #[test]
    fn test_refresh_allowance_positive_balance() {
        let state = PeerState::new(Au::from_amount(13_500_000), Au::from_amount(16_875_000));
        state.add_balance(Au::new(1000));

        let credit = refresh_allowance(&state, Au::from_amount(4_500_000));

        assert_eq!(credit, Au::new(0));
        assert_eq!(state.balance(), Au::new(1000));
    }

    #[test]
    fn test_refresh_allowance_negative_balance() {
        let state = PeerState::new(Au::from_amount(13_500_000), Au::from_amount(16_875_000));
        state.add_balance(Au::new(-1000));
        state.set_last_refresh(current_timestamp() - 1);

        let credit = refresh_allowance(&state, Au::from_amount(100));

        assert_eq!(credit, Au::new(100));
        assert_eq!(state.balance(), Au::new(-900));
    }

    #[test]
    fn test_refresh_allowance_caps_at_zero() {
        let state = PeerState::new(Au::from_amount(13_500_000), Au::from_amount(16_875_000));
        state.add_balance(Au::new(-100));
        state.set_last_refresh(current_timestamp() - 10);

        let credit = refresh_allowance(&state, Au::from_amount(1000));

        assert_eq!(credit, Au::new(100));
        assert_eq!(state.balance(), Au::new(0));
    }

    #[test]
    fn test_settlement_too_soon_same_second() {
        let state = PeerState::new(Au::from_amount(13_500_000), Au::from_amount(16_875_000));
        let now = current_timestamp();

        state.set_last_refresh(now);
        state.add_balance(Au::new(-10_000));

        let credit = refresh_allowance(&state, Au::from_amount(4_500_000));

        assert_eq!(credit, Au::new(0));
        assert_eq!(state.balance(), Au::new(-10_000));
    }

    #[test]
    fn test_time_limited_partial_acceptance() {
        let state = PeerState::new(Au::from_amount(13_500_000), Au::from_amount(16_875_000));
        state.add_balance(Au::new(-50_000));
        state.set_last_refresh(current_timestamp() - 3);

        let credit = refresh_allowance(&state, Au::from_amount(10_000));

        assert_eq!(credit, Au::new(30_000));
        assert_eq!(state.balance(), Au::new(-20_000));
    }

    #[test]
    fn test_full_debt_acceptance() {
        let state = PeerState::new(Au::from_amount(13_500_000), Au::from_amount(16_875_000));
        state.add_balance(Au::new(-10_000));
        state.set_last_refresh(current_timestamp() - 10);

        let credit = refresh_allowance(&state, Au::from_amount(10_000));

        assert_eq!(credit, Au::new(10_000));
        assert_eq!(state.balance(), Au::new(0));
    }

    #[test]
    fn test_sequential_refreshes() {
        let state = PeerState::new(Au::from_amount(13_500_000), Au::from_amount(16_875_000));
        let base_time = current_timestamp();

        state.add_balance(Au::new(-100_000));
        state.set_last_refresh(base_time - 5);

        let credit1 = refresh_allowance(&state, Au::from_amount(10_000));
        assert_eq!(credit1, Au::new(50_000));
        assert_eq!(state.balance(), Au::new(-50_000));

        let credit2 = refresh_allowance(&state, Au::from_amount(10_000));
        assert_eq!(credit2, Au::new(0));
        assert_eq!(state.balance(), Au::new(-50_000));

        state.set_last_refresh(current_timestamp() - 3);

        let credit3 = refresh_allowance(&state, Au::from_amount(10_000));
        assert_eq!(credit3, Au::new(30_000));
        assert_eq!(state.balance(), Au::new(-20_000));
    }

    #[test]
    fn test_refresh_allowance_does_not_overflow_into_a_tiny_allowance() {
        // A very large refresh rate over a long gap previously multiplied with a
        // raw saturating multiply: the checked scaling now bounds it instead of
        // wrapping into a small allowance. The credit is still capped at the
        // actual debt, so a large debt is fully forgiven and never under-credited
        // by a wrapped allowance.
        let state = PeerState::new(Au::from_amount(u64::MAX), Au::from_amount(u64::MAX));
        let debt = i64::MAX / 2;
        state.add_balance(Au::new(-debt));
        state.set_last_refresh(current_timestamp() - 1_000_000);

        let credit = refresh_allowance(&state, Au::from_amount(u64::MAX));

        // The whole debt is forgiven; the allowance never wrapped below it.
        assert_eq!(credit, Au::new(debt));
        assert_eq!(state.balance(), Au::ZERO);
    }

    #[test]
    fn test_first_refresh_initialization() {
        let state = PeerState::new(Au::from_amount(13_500_000), Au::from_amount(16_875_000));

        assert_eq!(state.last_refresh(), 0);
        state.add_balance(Au::new(-10_000));

        let credit = refresh_allowance(&state, Au::from_amount(10_000));

        assert_eq!(credit, Au::new(0));
        assert_eq!(state.balance(), Au::new(-10_000));
        assert!(state.last_refresh() > 0);
    }

    #[test]
    fn test_client_vs_storer_refresh_rate() {
        let storer_state = PeerState::new(Au::from_amount(13_500_000), Au::from_amount(16_875_000));
        let client_state = PeerState::new_client_only(
            Au::from_amount(13_500_000),
            Au::from_amount(16_875_000),
            10,
        );

        storer_state.add_balance(Au::new(-50_000));
        client_state.add_balance(Au::new(-50_000));
        storer_state.set_last_refresh(current_timestamp() - 10);
        client_state.set_last_refresh(current_timestamp() - 10);

        let storer_credit = refresh_allowance(&storer_state, Au::from_amount(10_000));
        assert_eq!(storer_credit, Au::new(50_000));
        assert_eq!(storer_state.balance(), Au::new(0));

        let client_credit = refresh_allowance(&client_state, Au::from_amount(1_000));
        assert_eq!(client_credit, Au::new(10_000));
        assert_eq!(client_state.balance(), Au::new(-40_000));
    }
}
