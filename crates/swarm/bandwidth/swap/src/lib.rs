//! Chequebook-based settlement provider for bandwidth accounting.
//!
//! When debt exceeds the payment threshold, the debtor issues a signed cheque.
//! The creditor stores cheques and can cash them on-chain at any time.
//!
//! # Usage
//!
//! Use [`create_swap_actor`] to create a service/handle pair.
//! The [`SwapProvider`] implements [`SwarmSettlementProvider`] for
//! integration with [`Accounting`].
//!
//! [`SwarmSettlementProvider`]: vertex_swarm_api::SwarmSettlementProvider

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

pub mod error;
pub mod handle;
pub mod service;

use std::sync::Arc;

use alloy_primitives::U256;
use tokio::sync::mpsc;
use vertex_swarm_api::{
    BandwidthMode, SwarmAccountingConfig, SwarmBandwidthAccounting, SwarmError, SwarmIdentity,
    SwarmPeerState, SwarmResult, SwarmSettlementProvider,
};
use vertex_swarm_bandwidth::{Accounting, AccountingPeerHandle};
use vertex_swarm_client::protocol::ClientCommand;
use vertex_swarm_primitives::OverlayAddress;

pub use error::SwapError;
pub use handle::SwapHandle;
pub use service::{SwapCommand, SwapService};
pub use vertex_swarm_client::SwapEvent;

/// Chequebook-based settlement provider.
///
/// On `settle()`, issues a cheque for outstanding debt exceeding the threshold.
/// With a handle, delegates to the network service; without, returns Ok(0).
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
        Self {
            config,
            handle: None,
        }
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
impl<C: SwarmAccountingConfig + 'static> SwarmSettlementProvider for SwapProvider<C> {
    fn supported_mode(&self) -> BandwidthMode {
        BandwidthMode::Swap
    }

    fn pre_allow(&self, _peer: OverlayAddress, _state: &dyn SwarmPeerState) -> i64 {
        // SWAP doesn't modify balance during allow check
        0
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
                %peer,
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

/// Create a swap actor (service + handle pair).
///
/// Spawn the service as a background task. Use the handle to create
/// a [`SwapProvider`].
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
    use vertex_swarm_api::{
        BandwidthMode, Direction, SwarmAccountingConfig, SwarmBandwidthAccounting, SwarmNodeType,
        SwarmPeerBandwidth,
    };
    use vertex_swarm_bandwidth::DefaultAccountingConfig;
    use vertex_swarm_bandwidth::PeerState;
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

        fn refresh_rate(&self) -> u64 {
            4_500_000
        }

        fn early_payment_percent(&self) -> u64 {
            50
        }

        fn client_only_factor(&self) -> u64 {
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
        let state = PeerState::new(13_500_000, 16_875_000);
        state.add_balance(-1000);

        let adjustment = provider.pre_allow(test_peer(), &state);

        assert_eq!(adjustment, 0);
        assert_eq!(state.balance(), -1000);
    }
}
