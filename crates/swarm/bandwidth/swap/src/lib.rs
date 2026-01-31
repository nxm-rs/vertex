//! SWAP - Chequebook-based settlement.
//!
//! This module implements chequebook-based settlement using Ethereum smart contracts.
//! When a peer's balance exceeds the payment threshold, a cheque is issued.
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
//! # Actor Pattern
//!
//! This crate implements the Handle+Service actor pattern:
//! - [`SwapService`] runs in its own tokio task and processes events
//! - [`SwapHandle`] is cheap-to-clone and used to send commands
//! - [`SwapProvider`] wraps the handle and implements [`SettlementProvider`]
//!
//! Use [`create_swap_actor`] to create the service and handle pair.
//!
//! # Provider Pattern
//!
//! This crate provides [`SwapProvider`] which implements the
//! [`SettlementProvider`](vertex_swarm_bandwidth::SettlementProvider) trait.
//! It can be composed with other providers (e.g., pseudosettle) using
//! [`Accounting`](vertex_swarm_bandwidth::Accounting).
//!
//! # Current Status
//!
//! The settlement logic is a stub. Full implementation will require:
//! - Chequebook contract deployment
//! - Cheque signing with the node's Ethereum key
//! - Cheque validation and storage
//! - On-chain settlement

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

pub mod error;
pub mod handle;
pub mod service;

use std::sync::Arc;

use alloy_primitives::U256;
use tokio::sync::mpsc;
use vertex_swarm_bandwidth::{AccountingError, Accounting, AccountingPeerHandle, PeerState, SettlementProvider};
use vertex_swarm_client::protocol::ClientCommand;
use vertex_swarm_api::{SwarmAccountingConfig, SwarmBandwidthAccounting, SwarmIdentity};

pub use error::SwapError;
pub use handle::SwapHandle;
pub use service::{SwapCommand, SwapService};
pub use vertex_swarm_client::SwapEvent;

/// SWAP provider - chequebook-based settlement.
///
/// This provider implements the SWAP settlement mechanism where peers settle
/// outstanding debt by issuing cheques. When `settle()` is called with a balance
/// exceeding the payment threshold, a cheque is issued for the outstanding amount.
///
/// # With Handle
///
/// When created with a handle (via [`create_swap_actor`]), the `settle()`
/// method delegates to the service for network-based settlement.
///
/// # Without Handle (Legacy)
///
/// When created with just a config, `settle()` logs and returns Ok(0).
pub struct SwapProvider<C> {
    config: C,
    /// Optional handle for delegating to the service.
    handle: Option<SwapHandle>,
}

impl<C: SwarmAccountingConfig> SwapProvider<C> {
    /// Create a new SWAP provider with the given configuration.
    ///
    /// This creates a provider without network settlement capability.
    /// Use [`create_swap_actor`] for full functionality.
    pub fn new(config: C) -> Self {
        Self { config, handle: None }
    }

    /// Create a new SWAP provider with a handle for network settlement.
    pub fn with_handle(config: C, handle: SwapHandle) -> Self {
        Self {
            config,
            handle: Some(handle),
        }
    }

    /// Get a reference to the configuration.
    pub fn config(&self) -> &C {
        &self.config
    }
}

#[async_trait::async_trait]
impl<C: SwarmAccountingConfig + 'static> SettlementProvider for SwapProvider<C> {
    fn pre_allow(&self, _state: &PeerState) -> i64 {
        // SWAP doesn't modify balance during allow check
        0
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
            // No handle - stub implementation
            let balance = state.balance();

            // Only settle if balance exceeds payment threshold (we owe them)
            if balance >= 0 {
                return Ok(0);
            }

            let debt = (-balance) as u64;
            let threshold = self.config.payment_threshold();

            if debt < threshold {
                return Ok(0);
            }

            tracing::debug!(
                peer = %state.peer(),
                balance = balance,
                debt = debt,
                "SWAP settlement stub - no-op (cheque would be issued here)"
            );

            // For now, just log and return 0 (no settlement occurred)
            Ok(0)
        }
    }

    fn name(&self) -> &'static str {
        "swap"
    }
}

/// Create a swap actor (service and handle pair).
///
/// This sets up the full swap functionality with network settlement.
/// The service should be spawned as a background task.
///
/// # Arguments
///
/// * `event_rx` - Receiver for events from the network layer
/// * `client_command_tx` - Sender for commands to the network layer
/// * `accounting` - Reference to the accounting system
/// * `our_rate` - Our exchange rate
///
/// # Returns
///
/// A tuple of (service, handle). Spawn the service and use the handle
/// to create a `SwapProvider`.
pub fn create_swap_actor<A: SwarmBandwidthAccounting + 'static>(
    event_rx: mpsc::UnboundedReceiver<SwapEvent>,
    client_command_tx: mpsc::UnboundedSender<ClientCommand>,
    accounting: Arc<A>,
    our_rate: U256,
) -> (SwapService<A>, SwapHandle) {
    let (command_tx, command_rx) = mpsc::unbounded_channel();

    let service = SwapService::new(
        command_rx,
        event_rx,
        client_command_tx,
        accounting,
        our_rate,
    );

    let handle = SwapHandle::new(command_tx);

    (service, handle)
}

/// Type alias for the SWAP peer handle.
pub type SwapPeerHandle = AccountingPeerHandle;

/// Create a new SWAP-only accounting instance.
///
/// This is a convenience function that creates a `Accounting` with
/// a `SwapProvider`.
pub fn new_swap_accounting<C: SwarmAccountingConfig + Clone + 'static, I: SwarmIdentity>(
    config: C,
    identity: I,
) -> Accounting<C, I> {
    Accounting::with_providers(
        config.clone(),
        identity,
        vec![Box::new(SwapProvider::new(config))],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use vertex_primitives::OverlayAddress;
    use vertex_swarm_api::{SwarmBandwidthAccounting, BandwidthMode, DefaultAccountingConfig, Direction, SwarmPeerBandwidth, SwarmNodeType, SwarmAccountingConfig};
    use vertex_swarm_identity::Identity;

    fn test_identity() -> Identity {
        Identity::random(vertex_swarmspec::init_testnet(), SwarmNodeType::Client)
    }

    struct SwapTestConfig;

    impl SwarmAccountingConfig for SwapTestConfig {
        fn mode(&self) -> BandwidthMode {
            BandwidthMode::Swap
        }

        fn payment_threshold(&self) -> u64 {
            13_500_000
        }

        fn payment_tolerance_percent(&self) -> u64 {
            25
        }

        fn base_price(&self) -> u64 {
            10_000
        }

        fn refresh_rate(&self) -> u64 {
            4_500_000
        }

        fn early_payment_percent(&self) -> u64 {
            50
        }

        fn light_factor(&self) -> u64 {
            10
        }
    }

    fn test_peer() -> OverlayAddress {
        OverlayAddress::from([1u8; 32])
    }

    #[test]
    fn test_swap_provider_name() {
        let provider = SwapProvider::new(SwapTestConfig);
        assert_eq!(provider.name(), "swap");
    }

    #[test]
    fn test_swap_accounting_basic() {
        let accounting = new_swap_accounting(DefaultAccountingConfig, test_identity());

        let handle = accounting.for_peer(test_peer());
        assert_eq!(handle.balance(), 0);

        handle.record(1000, Direction::Upload);
        assert_eq!(handle.balance(), 1000);

        handle.record(500, Direction::Download);
        assert_eq!(handle.balance(), 500);
    }

    #[test]
    fn test_swap_pre_allow_no_change() {
        let provider = SwapProvider::new(SwapTestConfig);
        let state = PeerState::new(test_peer(), 13_500_000, 16_875_000);
        state.add_balance(-1000);

        let adjustment = provider.pre_allow(&state);

        // SWAP doesn't adjust in pre_allow
        assert_eq!(adjustment, 0);
        assert_eq!(state.balance(), -1000);
    }
}
