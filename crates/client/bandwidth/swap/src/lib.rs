//! SWAP - Chequebook-based settlement (stub).
//!
//! This module will eventually implement chequebook-based settlement using
//! Ethereum smart contracts. For now, it's a stub that provides the type
//! structure for future implementation.
//!
//! # Design
//!
//! SWAP uses digital cheques signed with the node's Ethereum private key.
//! When a peer's balance exceeds the payment threshold:
//!
//! 1. The debtor creates and signs a cheque for the amount owed
//! 2. The creditor verifies and stores the cheque
//! 3. The creditor can cash the cheque on-chain at any time
//!
//! # Future Implementation
//!
//! The full implementation will require:
//! - Chequebook contract deployment
//! - Cheque signing with the node's Ethereum key
//! - Cheque validation and storage
//! - On-chain settlement
//!
//! This will be implemented in a separate `vertex-chequebook` crate.

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

use alloc::vec::Vec;
use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;

use vertex_bandwidth_core::{AccountingConfig, PeerState};
use vertex_primitives::OverlayAddress;
use vertex_swarm_api::{BandwidthAccounting, Direction, PeerBandwidth, SwarmResult};

/// SWAP accounting with chequebook-based settlement.
///
/// **NOTE**: This is currently a stub. Full implementation pending.
pub struct SwapAccounting {
    config: AccountingConfig,
    peers: RwLock<HashMap<OverlayAddress, Arc<SwapPeerState>>>,
}

/// Per-peer state for SWAP.
struct SwapPeerState {
    inner: PeerState,
}

impl SwapAccounting {
    /// Create a new SWAP accounting instance.
    pub fn new(config: AccountingConfig) -> Self {
        Self {
            config,
            peers: RwLock::new(HashMap::new()),
        }
    }

    /// Get the accounting configuration.
    pub fn config(&self) -> &AccountingConfig {
        &self.config
    }

    /// Get or create peer state.
    fn get_or_create_peer(&self, peer: OverlayAddress) -> Arc<SwapPeerState> {
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
                Arc::new(SwapPeerState {
                    inner: PeerState::new(
                        peer,
                        self.config.payment_threshold,
                        self.config.disconnect_threshold,
                    ),
                })
            })
            .clone()
    }
}

impl BandwidthAccounting for SwapAccounting {
    type Peer = SwapPeerHandle;

    fn for_peer(&self, peer: OverlayAddress) -> Self::Peer {
        let state = self.get_or_create_peer(peer);
        SwapPeerHandle {
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

/// Handle to a peer's SWAP accounting state.
#[derive(Clone)]
pub struct SwapPeerHandle {
    state: Arc<SwapPeerState>,
    config: AccountingConfig,
}

#[async_trait::async_trait]
impl PeerBandwidth for SwapPeerHandle {
    fn record(&self, bytes: u64, direction: Direction) {
        match direction {
            Direction::Upload => self.state.inner.add_balance(bytes as i64),
            Direction::Download => self.state.inner.add_balance(-(bytes as i64)),
        }
    }

    fn allow(&self, bytes: u64) -> bool {
        let balance = self.state.inner.balance();
        let reserved = self.state.inner.reserved_balance();
        let projected = balance - (bytes as i64) - (reserved as i64);

        projected >= -(self.config.disconnect_threshold as i64)
    }

    fn balance(&self) -> i64 {
        self.state.inner.balance()
    }

    async fn settle(&self) -> SwarmResult<()> {
        tracing::debug!(
            peer = %self.state.inner.peer(),
            balance = self.state.inner.balance(),
            "SWAP settlement stub - no-op"
        );
        Ok(())
    }

    fn peer(&self) -> OverlayAddress {
        self.state.inner.peer()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_peer() -> OverlayAddress {
        OverlayAddress::from([1u8; 32])
    }

    #[test]
    fn test_swap_basic() {
        let accounting = SwapAccounting::new(AccountingConfig::default());

        let handle = accounting.for_peer(test_peer());
        assert_eq!(handle.balance(), 0);

        handle.record(1000, Direction::Upload);
        assert_eq!(handle.balance(), 1000);

        handle.record(500, Direction::Download);
        assert_eq!(handle.balance(), 500);
    }

    #[tokio::test]
    async fn test_swap_settle_stub() {
        let accounting = SwapAccounting::new(AccountingConfig::default());
        let handle = accounting.for_peer(test_peer());

        handle.settle().await.unwrap();
    }
}
