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
//! - Light nodes receive a reduced refresh rate (e.g., 1/5th)

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

use alloc::vec::Vec;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::RwLock;

use vertex_bandwidth_core::{
    AccountingConfig, AccountingError, CreditAction, DEFAULT_REFRESH_RATE, DebitAction, PeerState,
};
use vertex_primitives::OverlayAddress;
use vertex_swarm_api::{BandwidthAccounting, Direction, PeerBandwidth, SwarmResult};

/// Pseudosettle accounting with time-based allowance.
///
/// Wraps the base accounting with periodic balance refresh.
pub struct PseudosettleAccounting {
    config: AccountingConfig,
    refresh_rate: u64,
    peers: RwLock<HashMap<OverlayAddress, Arc<PseudosettlePeerState>>>,
}

/// Per-peer state for pseudosettle.
struct PseudosettlePeerState {
    inner: PeerState,
    refresh_rate: u64,
}

impl PseudosettleAccounting {
    /// Create a new pseudosettle accounting instance.
    pub fn new(config: AccountingConfig, refresh_rate: u64) -> Self {
        Self {
            config,
            refresh_rate,
            peers: RwLock::new(HashMap::new()),
        }
    }

    /// Create with default refresh rate.
    pub fn with_default_refresh(config: AccountingConfig) -> Self {
        Self::new(config, DEFAULT_REFRESH_RATE)
    }

    /// Get the refresh rate.
    pub fn refresh_rate(&self) -> u64 {
        self.refresh_rate
    }

    /// Get the accounting configuration.
    pub fn config(&self) -> &AccountingConfig {
        &self.config
    }

    /// Prepare a credit action with refresh check.
    pub fn prepare_credit(
        &self,
        peer: OverlayAddress,
        price: u64,
        _originated: bool,
    ) -> Result<CreditAction, AccountingError> {
        let state = self.get_or_create_peer(peer);

        // Refresh the allowance first
        self.refresh_allowance(&state);

        // Check if we can afford this credit
        let current_balance = state.inner.balance();
        let reserved = state.inner.reserved_balance();
        let projected = current_balance - (price as i64) - (reserved as i64);

        let threshold = -(self.config.disconnect_threshold as i64);
        if projected < threshold {
            return Err(AccountingError::DisconnectThreshold {
                peer,
                balance: current_balance,
                threshold: self.config.disconnect_threshold,
            });
        }

        // Reserve the balance
        state.inner.add_reserved(price);

        // Return action with inner state
        Ok(CreditAction::new(
            Arc::new(PeerState::new(
                peer,
                self.config.payment_threshold,
                self.config.disconnect_threshold,
            )),
            price,
        ))
    }

    /// Prepare a debit action.
    pub fn prepare_debit(
        &self,
        peer: OverlayAddress,
        price: u64,
    ) -> Result<DebitAction, AccountingError> {
        let state = self.get_or_create_peer(peer);

        // Reserve shadow balance
        state.inner.add_shadow_reserved(price);

        Ok(DebitAction::new(
            Arc::new(PeerState::new(
                peer,
                self.config.payment_threshold,
                self.config.disconnect_threshold,
            )),
            price,
        ))
    }

    /// Refresh the time-based allowance for a peer.
    fn refresh_allowance(&self, state: &PseudosettlePeerState) {
        let now = current_timestamp();
        let last = state.inner.last_refresh();

        if last == 0 {
            state.inner.set_last_refresh(now);
            return;
        }

        let elapsed = now.saturating_sub(last);
        if elapsed == 0 {
            return;
        }

        // Calculate allowance: elapsed_seconds * refresh_rate
        let allowance = (elapsed as u64) * state.refresh_rate;

        // Add to balance (moving towards positive = peer owes us less)
        // Only add if balance is negative (we owe them)
        let balance = state.inner.balance();
        if balance < 0 {
            let credit = (allowance as i64).min(-balance);
            state.inner.add_balance(credit);
        }

        state.inner.set_last_refresh(now);
    }

    /// Get or create peer state.
    fn get_or_create_peer(&self, peer: OverlayAddress) -> Arc<PseudosettlePeerState> {
        {
            let peers = self.peers.read();
            if let Some(state) = peers.get(&peer) {
                return Arc::clone(state);
            }
        }

        let mut peers = self.peers.write();
        peers
            .entry(peer)
            .or_insert_with(|| {
                Arc::new(PseudosettlePeerState {
                    inner: PeerState::new(
                        peer,
                        self.config.payment_threshold,
                        self.config.disconnect_threshold,
                    ),
                    refresh_rate: self.refresh_rate,
                })
            })
            .clone()
    }
}

impl BandwidthAccounting for PseudosettleAccounting {
    type Peer = PseudosettlePeerHandle;

    fn for_peer(&self, peer: OverlayAddress) -> Self::Peer {
        let state = self.get_or_create_peer(peer);
        PseudosettlePeerHandle {
            state,
            config: self.config.clone(),
        }
    }

    fn peers(&self) -> Vec<OverlayAddress> {
        self.peers.read().keys().copied().collect()
    }

    fn remove_peer(&self, peer: &OverlayAddress) {
        self.peers.write().remove(peer);
    }
}

/// Handle to a peer's pseudosettle accounting state.
#[derive(Clone)]
pub struct PseudosettlePeerHandle {
    state: Arc<PseudosettlePeerState>,
    config: AccountingConfig,
}

#[async_trait::async_trait]
impl PeerBandwidth for PseudosettlePeerHandle {
    fn record(&self, bytes: u64, direction: Direction) {
        match direction {
            Direction::Upload => self.state.inner.add_balance(bytes as i64),
            Direction::Download => self.state.inner.add_balance(-(bytes as i64)),
        }
    }

    fn allow(&self, bytes: u64) -> bool {
        refresh_peer_allowance(&self.state);

        let balance = self.state.inner.balance();
        let reserved = self.state.inner.reserved_balance();
        let projected = balance - (bytes as i64) - (reserved as i64);

        projected >= -(self.config.disconnect_threshold as i64)
    }

    fn balance(&self) -> i64 {
        self.state.inner.balance()
    }

    async fn settle(&self) -> SwarmResult<()> {
        // Pseudosettle doesn't require explicit settlement.
        // Refresh happens automatically on allow() checks.
        Ok(())
    }

    fn peer(&self) -> OverlayAddress {
        self.state.inner.peer()
    }
}

/// Refresh the peer's allowance.
fn refresh_peer_allowance(state: &PseudosettlePeerState) {
    let now = current_timestamp();
    let last = state.inner.last_refresh();

    if last == 0 {
        state.inner.set_last_refresh(now);
        return;
    }

    let elapsed = now.saturating_sub(last);
    if elapsed == 0 {
        return;
    }

    let allowance = (elapsed as u64) * state.refresh_rate;
    let balance = state.inner.balance();

    if balance < 0 {
        let credit = (allowance as i64).min(-balance);
        state.inner.add_balance(credit);
    }

    state.inner.set_last_refresh(now);
}

/// Get current timestamp in seconds.
fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_peer() -> OverlayAddress {
        OverlayAddress::from([1u8; 32])
    }

    #[test]
    fn test_pseudosettle_basic() {
        let accounting = PseudosettleAccounting::with_default_refresh(AccountingConfig::default());

        let handle = accounting.for_peer(test_peer());
        assert_eq!(handle.balance(), 0);

        handle.record(1000, Direction::Upload);
        assert_eq!(handle.balance(), 1000);

        handle.record(500, Direction::Download);
        assert_eq!(handle.balance(), 500);
    }

    #[test]
    fn test_refresh_rate() {
        let config = AccountingConfig::default();
        let accounting = PseudosettleAccounting::new(config, 100);

        assert_eq!(accounting.refresh_rate(), 100);
    }
}
