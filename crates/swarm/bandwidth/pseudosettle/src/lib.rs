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

pub mod error;
pub mod handle;
pub mod service;

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc;
use vertex_swarm_api::{
    BandwidthMode, SwarmAccountingConfig, SwarmBandwidthAccounting, SwarmError, SwarmIdentity,
    SwarmPeerState, SwarmResult, SwarmSettlementProvider,
};
use vertex_swarm_bandwidth::{Accounting, AccountingPeerHandle};
use vertex_swarm_client::protocol::ClientCommand;
use vertex_swarm_primitives::OverlayAddress;

pub use error::PseudosettleError;
pub use handle::PseudosettleHandle;
pub use service::{PseudosettleCommand, PseudosettleService};
pub use vertex_swarm_client::PseudosettleEvent;

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

    fn pre_allow(&self, _peer: OverlayAddress, state: &dyn SwarmPeerState) -> i64 {
        refresh_allowance(state, self.config.refresh_rate())
    }

    async fn settle(&self, peer: OverlayAddress, state: &dyn SwarmPeerState) -> SwarmResult<i64> {
        // If we have a handle, delegate to the service
        if let Some(handle) = &self.handle {
            let balance = state.balance();
            if balance >= 0 {
                return Ok(0); // Nothing to settle
            }

            let amount = (-balance) as u64;
            let accepted =
                handle
                    .settle(peer, amount)
                    .await
                    .map_err(|e| SwarmError::PaymentRequired {
                        reason: e.to_string(),
                    })?;

            Ok(accepted as i64)
        } else {
            // No handle - refresh happens automatically in pre_allow()
            Ok(0)
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

/// Apply time-based credit to peer's negative balance. Returns credit applied.
fn refresh_allowance(state: &dyn SwarmPeerState, refresh_rate: u64) -> i64 {
    let now = current_timestamp();
    let last = state.last_refresh();

    if last == 0 {
        state.set_last_refresh(now);
        return 0;
    }

    let elapsed = now.saturating_sub(last);
    if elapsed == 0 {
        return 0;
    }

    // Calculate allowance: elapsed_seconds * refresh_rate
    let allowance = elapsed * refresh_rate;

    // Only add credit if balance is negative (we owe them)
    let balance = state.balance();
    let credit = if balance < 0 {
        let credit = (allowance as i64).min(-balance);
        state.add_balance(credit);
        credit
    } else {
        0
    };

    state.set_last_refresh(now);
    credit
}

/// Get current timestamp in seconds.
fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Type alias for the pseudosettle peer handle.
pub type PseudosettlePeerHandle = AccountingPeerHandle;

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
    use vertex_swarm_api::{
        Direction, SwarmBandwidthAccounting, SwarmNodeType, SwarmPeerBandwidth,
    };
    use vertex_swarm_bandwidth::DefaultAccountingConfig;
    use vertex_swarm_bandwidth::PeerState;
    use vertex_swarm_identity::Identity;

    fn test_identity() -> Identity {
        Identity::random(vertex_swarmspec::init_testnet(), SwarmNodeType::Client)
    }

    fn test_peer() -> OverlayAddress {
        OverlayAddress::from([1u8; 32])
    }

    #[test]
    fn test_pseudosettle_provider_name() {
        let provider = PseudosettleProvider::new(DefaultAccountingConfig);
        assert_eq!(provider.name(), "pseudosettle");
    }

    #[test]
    fn test_pseudosettle_refresh_rate() {
        let provider = PseudosettleProvider::new(DefaultAccountingConfig);
        assert_eq!(provider.refresh_rate(), 4_500_000);
    }

    #[test]
    fn test_pseudosettle_accounting_basic() {
        let accounting = new_pseudosettle_accounting(DefaultAccountingConfig, test_identity());

        let handle = accounting.for_peer(test_peer());
        assert_eq!(handle.balance(), 0);

        handle.record(1000, Direction::Upload);
        assert_eq!(handle.balance(), 1000);

        handle.record(500, Direction::Download);
        assert_eq!(handle.balance(), 500);
    }

    #[test]
    fn test_refresh_allowance_positive_balance() {
        let state = PeerState::new(13_500_000, 16_875_000);
        state.add_balance(1000);

        let credit = refresh_allowance(&state, 4_500_000);

        assert_eq!(credit, 0);
        assert_eq!(state.balance(), 1000);
    }

    #[test]
    fn test_refresh_allowance_negative_balance() {
        let state = PeerState::new(13_500_000, 16_875_000);
        state.add_balance(-1000);
        state.set_last_refresh(current_timestamp() - 1);

        let credit = refresh_allowance(&state, 100);

        assert_eq!(credit, 100);
        assert_eq!(state.balance(), -900);
    }

    #[test]
    fn test_refresh_allowance_caps_at_zero() {
        let state = PeerState::new(13_500_000, 16_875_000);
        state.add_balance(-100);
        state.set_last_refresh(current_timestamp() - 10);

        let credit = refresh_allowance(&state, 1000);

        assert_eq!(credit, 100);
        assert_eq!(state.balance(), 0);
    }

    #[test]
    fn test_settlement_too_soon_same_second() {
        let state = PeerState::new(13_500_000, 16_875_000);
        let now = current_timestamp();

        state.set_last_refresh(now);
        state.add_balance(-10_000);

        let credit = refresh_allowance(&state, 4_500_000);

        assert_eq!(credit, 0);
        assert_eq!(state.balance(), -10_000);
    }

    #[test]
    fn test_time_limited_partial_acceptance() {
        let state = PeerState::new(13_500_000, 16_875_000);
        state.add_balance(-50_000);
        state.set_last_refresh(current_timestamp() - 3);

        let credit = refresh_allowance(&state, 10_000);

        assert_eq!(credit, 30_000);
        assert_eq!(state.balance(), -20_000);
    }

    #[test]
    fn test_full_debt_acceptance() {
        let state = PeerState::new(13_500_000, 16_875_000);
        state.add_balance(-10_000);
        state.set_last_refresh(current_timestamp() - 10);

        let credit = refresh_allowance(&state, 10_000);

        assert_eq!(credit, 10_000);
        assert_eq!(state.balance(), 0);
    }

    #[test]
    fn test_sequential_refreshes() {
        let state = PeerState::new(13_500_000, 16_875_000);
        let base_time = current_timestamp();

        state.add_balance(-100_000);
        state.set_last_refresh(base_time - 5);

        let credit1 = refresh_allowance(&state, 10_000);
        assert_eq!(credit1, 50_000);
        assert_eq!(state.balance(), -50_000);

        let credit2 = refresh_allowance(&state, 10_000);
        assert_eq!(credit2, 0);
        assert_eq!(state.balance(), -50_000);

        state.set_last_refresh(current_timestamp() - 3);

        let credit3 = refresh_allowance(&state, 10_000);
        assert_eq!(credit3, 30_000);
        assert_eq!(state.balance(), -20_000);
    }

    #[test]
    fn test_first_refresh_initialization() {
        let state = PeerState::new(13_500_000, 16_875_000);

        assert_eq!(state.last_refresh(), 0);
        state.add_balance(-10_000);

        let credit = refresh_allowance(&state, 10_000);

        assert_eq!(credit, 0);
        assert_eq!(state.balance(), -10_000);
        assert!(state.last_refresh() > 0);
    }

    #[test]
    fn test_client_vs_storer_refresh_rate() {
        let storer_state = PeerState::new(13_500_000, 16_875_000);
        let client_state = PeerState::new_client_only(13_500_000, 16_875_000, 10);

        storer_state.add_balance(-50_000);
        client_state.add_balance(-50_000);
        storer_state.set_last_refresh(current_timestamp() - 10);
        client_state.set_last_refresh(current_timestamp() - 10);

        let storer_credit = refresh_allowance(&storer_state, 10_000);
        assert_eq!(storer_credit, 50_000);
        assert_eq!(storer_state.balance(), 0);

        let client_credit = refresh_allowance(&client_state, 1_000);
        assert_eq!(client_credit, 10_000);
        assert_eq!(client_state.balance(), -40_000);
    }
}
