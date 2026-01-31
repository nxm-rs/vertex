//! Pseudosettle - time-based bandwidth settlement without blockchain.
//!
//! Pseudosettle provides a simple settlement mechanism where peers are granted
//! a time-based allowance that refreshes periodically. This allows bandwidth
//! usage without requiring blockchain transactions.
//!
//! # Design
//!
//! - Each peer accumulates a "refresh" allowance over time
//! - The allowance is added to their balance when they would otherwise be disconnected
//! - Light nodes receive a reduced refresh rate (e.g., 1/10th)
//!
//! # Actor Pattern
//!
//! This crate implements the Handle+Service actor pattern:
//! - [`PseudosettleService`] runs in its own tokio task and processes events
//! - [`PseudosettleHandle`] is cheap-to-clone and used to send commands
//! - [`PseudosettleProvider`] wraps the handle and implements [`SettlementProvider`]
//!
//! Use [`create_pseudosettle_actor`] to create the service and handle pair.
//!
//! # Provider Pattern
//!
//! This crate provides [`PseudosettleProvider`] which implements the
//! [`SettlementProvider`](vertex_swarm_bandwidth::SettlementProvider) trait.
//! It can be composed with other providers (e.g., swap) using
//! [`Accounting`](vertex_swarm_bandwidth::Accounting).

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

pub mod error;
pub mod handle;
pub mod service;

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc;
use vertex_swarm_bandwidth::{AccountingError, Accounting, AccountingPeerHandle, PeerState, SettlementProvider};
use vertex_swarm_client::protocol::ClientCommand;
use vertex_swarm_api::{SwarmAccountingConfig, SwarmBandwidthAccounting, SwarmIdentity};

pub use error::PseudosettleError;
pub use handle::PseudosettleHandle;
pub use service::{PseudosettleCommand, PseudosettleService};
pub use vertex_swarm_client::PseudosettleEvent;

/// Pseudosettle provider - time-based debt forgiveness.
///
/// This provider implements the pseudosettle mechanism where peers are granted
/// a time-based allowance that refreshes periodically. When `pre_allow()` is
/// called, any negative balance is reduced based on elapsed time since the
/// last refresh.
///
/// # Refresh Formula
///
/// ```text
/// allowance = elapsed_seconds × refresh_rate
/// credit = min(allowance, abs(negative_balance))
/// ```
///
/// # With Handle
///
/// When created with a handle (via [`create_pseudosettle_actor`]), the `settle()`
/// method delegates to the service for network-based settlement.
///
/// # Without Handle (Legacy)
///
/// When created with just a config, `settle()` returns Ok(0) and only local
/// refresh via `pre_allow()` is performed.
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
        Self { config, handle: None }
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
impl<C: SwarmAccountingConfig + 'static> SettlementProvider for PseudosettleProvider<C> {
    fn pre_allow(&self, state: &PeerState) -> i64 {
        refresh_allowance(state, self.config.refresh_rate())
    }

    async fn settle(&self, state: &PeerState) -> Result<i64, AccountingError> {
        // If we have a handle, delegate to the service
        if let Some(handle) = &self.handle {
            let balance = state.balance();
            if balance >= 0 {
                return Ok(0); // Nothing to settle
            }

            let amount = (-balance) as u64;
            let accepted = handle
                .settle(state.peer(), amount)
                .await
                .map_err(|e| AccountingError::SettlementFailed(e.to_string()))?;

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

/// Create a pseudosettle actor (service and handle pair).
///
/// This sets up the full pseudosettle functionality with network settlement.
/// The service should be spawned as a background task.
///
/// # Arguments
///
/// * `event_rx` - Receiver for events from the network layer
/// * `client_command_tx` - Sender for commands to the network layer
/// * `accounting` - Reference to the accounting system
/// * `refresh_rate` - Tokens per second for time-based allowance
///
/// # Returns
///
/// A tuple of (service, handle). Spawn the service and use the handle
/// to create a `PseudosettleProvider`.
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

/// Refresh the time-based allowance for a peer.
///
/// Returns the amount of credit applied (0 if no refresh was needed).
fn refresh_allowance(state: &PeerState, refresh_rate: u64) -> i64 {
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
    let allowance = (elapsed as u64) * refresh_rate;

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
    use vertex_primitives::OverlayAddress;
    use vertex_swarm_api::{SwarmBandwidthAccounting, DefaultAccountingConfig, Direction, SwarmPeerBandwidth, SwarmNodeType};
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
        let state = PeerState::new(test_peer(), 13_500_000, 16_875_000);
        state.add_balance(1000); // positive balance

        let credit = refresh_allowance(&state, 4_500_000);

        // No credit should be applied (balance is positive)
        assert_eq!(credit, 0);
        assert_eq!(state.balance(), 1000);
    }

    #[test]
    fn test_refresh_allowance_negative_balance() {
        let state = PeerState::new(test_peer(), 13_500_000, 16_875_000);
        state.add_balance(-1000); // negative balance
        state.set_last_refresh(current_timestamp() - 1); // 1 second ago

        let credit = refresh_allowance(&state, 100); // 100 AU/sec

        // Credit should be min(100, 1000) = 100
        assert_eq!(credit, 100);
        assert_eq!(state.balance(), -900);
    }

    #[test]
    fn test_refresh_allowance_caps_at_zero() {
        let state = PeerState::new(test_peer(), 13_500_000, 16_875_000);
        state.add_balance(-100); // small negative balance
        state.set_last_refresh(current_timestamp() - 10); // 10 seconds ago

        let credit = refresh_allowance(&state, 1000); // 1000 AU/sec = 10000 total

        // Credit should be capped at abs(balance) = 100
        assert_eq!(credit, 100);
        assert_eq!(state.balance(), 0);
    }

    /// Test: Settlement too soon (same second) returns zero credit.
    /// Matches Bee's behavior where `currentTime == lastTime.Timestamp` returns error.
    #[test]
    fn test_settlement_too_soon_same_second() {
        let state = PeerState::new(test_peer(), 13_500_000, 16_875_000);
        let now = current_timestamp();

        // Set last refresh to current time (same second)
        state.set_last_refresh(now);
        state.add_balance(-10_000); // We owe peer 10,000 AU

        // Attempt refresh - should return 0 because elapsed == 0
        let credit = refresh_allowance(&state, 4_500_000);

        assert_eq!(credit, 0, "No credit should be applied when elapsed time is 0");
        assert_eq!(state.balance(), -10_000, "Balance should be unchanged");
    }

    /// Test: Time-based rate limiting - partial acceptance.
    /// Matches Bee's TestTimeLimitedPayment scenario where debt > time-based allowance.
    #[test]
    fn test_time_limited_partial_acceptance() {
        let refresh_rate: u64 = 10_000; // 10,000 AU per second
        let state = PeerState::new(test_peer(), 13_500_000, 16_875_000);

        // Peer owes us nothing, we owe them 50,000 AU
        state.add_balance(-50_000);

        // 3 seconds elapsed
        state.set_last_refresh(current_timestamp() - 3);

        // Allowance should be 3 * 10,000 = 30,000 AU
        // But debt is 50,000, so credit should be min(30,000, 50,000) = 30,000
        let credit = refresh_allowance(&state, refresh_rate);

        assert_eq!(credit, 30_000, "Credit should be limited to time-based allowance");
        assert_eq!(state.balance(), -20_000, "Balance should be debt minus credit");
    }

    /// Test: Full debt acceptance when allowance >= debt.
    /// Matches Bee's TestPayment basic scenario.
    #[test]
    fn test_full_debt_acceptance() {
        let refresh_rate: u64 = 10_000;
        let state = PeerState::new(test_peer(), 13_500_000, 16_875_000);

        // We owe peer 10,000 AU
        state.add_balance(-10_000);

        // 10 seconds elapsed = 100,000 AU allowance
        state.set_last_refresh(current_timestamp() - 10);

        // Allowance (100,000) > debt (10,000), so full debt should be credited
        let credit = refresh_allowance(&state, refresh_rate);

        assert_eq!(credit, 10_000, "Full debt should be credited when allowance exceeds debt");
        assert_eq!(state.balance(), 0, "Balance should reach zero");
    }

    /// Test: Multiple sequential refreshes with time progression.
    /// Matches Bee's TestTimeLimitedPayment multi-step scenario.
    #[test]
    fn test_sequential_refreshes() {
        let refresh_rate: u64 = 10_000;
        let state = PeerState::new(test_peer(), 13_500_000, 16_875_000);
        let base_time = current_timestamp();

        // Initial state: we owe 100,000 AU
        state.add_balance(-100_000);
        state.set_last_refresh(base_time - 5); // 5 seconds ago

        // First refresh: 5 seconds = 50,000 AU credit
        let credit1 = refresh_allowance(&state, refresh_rate);
        assert_eq!(credit1, 50_000);
        assert_eq!(state.balance(), -50_000);

        // Immediately try again (same second) - should get 0
        let credit2 = refresh_allowance(&state, refresh_rate);
        assert_eq!(credit2, 0, "No credit on same-second refresh");
        assert_eq!(state.balance(), -50_000);

        // Simulate 3 more seconds passing
        state.set_last_refresh(current_timestamp() - 3);

        // Third refresh: 3 seconds = 30,000 AU credit
        let credit3 = refresh_allowance(&state, refresh_rate);
        assert_eq!(credit3, 30_000);
        assert_eq!(state.balance(), -20_000);
    }

    /// Test: First refresh initializes timestamp without crediting.
    #[test]
    fn test_first_refresh_initialization() {
        let state = PeerState::new(test_peer(), 13_500_000, 16_875_000);

        // No last_refresh set (0)
        assert_eq!(state.last_refresh(), 0);
        state.add_balance(-10_000);

        // First refresh should just set the timestamp
        let credit = refresh_allowance(&state, 10_000);

        assert_eq!(credit, 0, "First refresh should not credit");
        assert_eq!(state.balance(), -10_000, "Balance should be unchanged");
        assert!(state.last_refresh() > 0, "Timestamp should be initialized");
    }

    /// Test: Light node vs full node refresh rate simulation.
    /// In Bee, light nodes get 1/10th the refresh rate.
    #[test]
    fn test_light_vs_full_refresh_rate() {
        let full_refresh_rate: u64 = 10_000;
        let light_refresh_rate: u64 = 1_000; // 1/10th

        let full_state = PeerState::new(test_peer(), 13_500_000, 16_875_000);
        let light_state = PeerState::new_light(test_peer(), 13_500_000, 16_875_000, 10);

        // Both owe 50,000 AU, 10 seconds elapsed
        full_state.add_balance(-50_000);
        light_state.add_balance(-50_000);
        full_state.set_last_refresh(current_timestamp() - 10);
        light_state.set_last_refresh(current_timestamp() - 10);

        // Full node: 10 * 10,000 = 100,000 allowance, debt is 50,000 -> credit 50,000
        let full_credit = refresh_allowance(&full_state, full_refresh_rate);
        assert_eq!(full_credit, 50_000);
        assert_eq!(full_state.balance(), 0);

        // Light node: 10 * 1,000 = 10,000 allowance, debt is 50,000 -> credit 10,000
        let light_credit = refresh_allowance(&light_state, light_refresh_rate);
        assert_eq!(light_credit, 10_000);
        assert_eq!(light_state.balance(), -40_000);
    }
}
