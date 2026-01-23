//! Combined availability accounting implementations.
//!
//! This module provides the `PseudosettleSwap` type that combines both
//! pseudosettle and SWAP settlement mechanisms. This is the default mode
//! for mainnet nodes.
//!
//! # Settlement Priority
//!
//! When both mechanisms are enabled:
//! 1. Pseudosettle provides a free "refresh" allowance over time
//! 2. When that's exhausted, SWAP cheques are used for payment
//!
//! This allows light traffic to flow freely while heavier usage
//! requires actual payment through the chequebook system.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::RwLock;

use vertex_bandwidth_core::{AccountingConfig, PeerState};
use vertex_primitives::OverlayAddress;
use vertex_swarm_api::{AvailabilityAccounting, Direction, PeerAvailability, SwarmResult};

/// Combined pseudosettle + SWAP availability accounting.
///
/// This is the recommended mode for mainnet nodes. It provides:
/// - Time-based refresh allowance (pseudosettle) for light usage
/// - Chequebook settlement (SWAP) when refresh is exhausted
///
/// # Example
///
/// ```ignore
/// use vertex_node_core::availability::PseudosettleSwap;
///
/// // Create combined accounting
/// let accounting = PseudosettleSwap::new(config, refresh_rate);
///
/// // Use trait interface
/// let peer_handle = accounting.for_peer(peer_addr);
/// peer_handle.record(1024, Direction::Download);
/// ```
pub struct PseudosettleSwap {
    config: AccountingConfig,
    refresh_rate: u64,
    peers: RwLock<HashMap<OverlayAddress, Arc<CombinedPeerState>>>,
}

/// Per-peer state for combined accounting.
struct CombinedPeerState {
    inner: PeerState,
    refresh_rate: u64,
}

impl PseudosettleSwap {
    /// Create a new combined accounting instance.
    pub fn new(config: AccountingConfig, refresh_rate: u64) -> Self {
        Self {
            config,
            refresh_rate,
            peers: RwLock::new(HashMap::new()),
        }
    }

    /// Create with default refresh rate.
    pub fn with_default_refresh(config: AccountingConfig) -> Self {
        Self::new(config, vertex_bandwidth_core::DEFAULT_REFRESH_RATE)
    }

    /// Get the refresh rate.
    pub fn refresh_rate(&self) -> u64 {
        self.refresh_rate
    }

    /// Get the accounting configuration.
    pub fn config(&self) -> &AccountingConfig {
        &self.config
    }

    /// Get or create peer state.
    fn get_or_create_peer(&self, peer: OverlayAddress) -> Arc<CombinedPeerState> {
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
                Arc::new(CombinedPeerState {
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

impl AvailabilityAccounting for PseudosettleSwap {
    type Peer = PseudosettleSwapPeerHandle;

    fn for_peer(&self, peer: OverlayAddress) -> Self::Peer {
        let state = self.get_or_create_peer(peer);
        PseudosettleSwapPeerHandle {
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

/// Handle to a peer's combined accounting state.
#[derive(Clone)]
pub struct PseudosettleSwapPeerHandle {
    state: Arc<CombinedPeerState>,
    config: AccountingConfig,
}

#[async_trait]
impl PeerAvailability for PseudosettleSwapPeerHandle {
    fn record(&self, bytes: u64, direction: Direction) {
        match direction {
            Direction::Upload => self.state.inner.add_balance(bytes as i64),
            Direction::Download => self.state.inner.add_balance(-(bytes as i64)),
        }
    }

    fn allow(&self, bytes: u64) -> bool {
        // First try pseudosettle refresh
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
        // Combined settlement:
        // 1. First apply any pseudosettle refresh
        refresh_peer_allowance(&self.state);

        // 2. If still over threshold, would trigger SWAP settlement
        let balance = self.state.inner.balance();
        if balance < -(self.config.payment_threshold as i64) {
            tracing::debug!(
                peer = %self.state.inner.peer(),
                balance = balance,
                "SWAP settlement would be triggered (stub)"
            );
            // TODO: Actual SWAP cheque issuance
        }

        Ok(())
    }

    fn peer(&self) -> OverlayAddress {
        self.state.inner.peer()
    }
}

/// Refresh the peer's time-based allowance.
fn refresh_peer_allowance(state: &CombinedPeerState) {
    use std::time::{SystemTime, UNIX_EPOCH};

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let last = state.inner.last_refresh();

    if last == 0 {
        state.inner.set_last_refresh(now);
        return;
    }

    let elapsed = now.saturating_sub(last);
    if elapsed == 0 {
        return;
    }

    let allowance = elapsed * state.refresh_rate;
    let balance = state.inner.balance();

    // Only credit if we owe them (balance is negative)
    if balance < 0 {
        let credit = (allowance as i64).min(-balance);
        state.inner.add_balance(credit);
    }

    state.inner.set_last_refresh(now);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_peer() -> OverlayAddress {
        OverlayAddress::from([1u8; 32])
    }

    #[test]
    fn test_combined_basic() {
        let accounting = PseudosettleSwap::with_default_refresh(AccountingConfig::default());

        let handle = accounting.for_peer(test_peer());
        assert_eq!(handle.balance(), 0);

        handle.record(1000, Direction::Upload);
        assert_eq!(handle.balance(), 1000);

        handle.record(500, Direction::Download);
        assert_eq!(handle.balance(), 500);
    }

    #[test]
    fn test_combined_implements_trait() {
        let accounting = PseudosettleSwap::with_default_refresh(AccountingConfig::default());

        // Should implement AvailabilityAccounting
        let _peers = accounting.peers();
        let handle = accounting.for_peer(test_peer());

        // Handle should implement PeerAvailability
        assert!(handle.allow(1000));
    }

    #[test]
    fn test_refresh_rate() {
        let config = AccountingConfig::default();
        let accounting = PseudosettleSwap::new(config, 100);

        assert_eq!(accounting.refresh_rate(), 100);
    }
}
