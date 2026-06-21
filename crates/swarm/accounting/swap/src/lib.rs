//! Chequebook-based settlement provider for bandwidth accounting.
//!
//! When debt crosses the payment threshold, the debtor issues a signed cheque
//! whose cumulative payout advances monotonically. The creditor validates the
//! cheque (signature recovers to the expected issuer, payout strictly increases)
//! and credits the incremental amount. Cheque exchange is fully chain-free; the
//! optional `swap-chequebook` feature adds an on-chain client for redeeming
//! received cheques.
//!
//! Swap is one of the pluggable [`SwarmSettlementProvider`]s, alongside
//! pseudosettle. The two compose: pseudosettle forgives debt up to a time-based
//! allowance, swap pays the remainder.
//!
//! # Usage
//!
//! Use [`create_swap_actor`] to create a service/handle pair. Spawn the service
//! as a background task and build a [`SwapProvider`] from the handle.
//!
//! [`SwarmSettlementProvider`]: vertex_swarm_api::SwarmSettlementProvider

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

#[cfg(feature = "swap-chequebook")]
pub mod cashout;
pub mod constants;
pub mod error;
pub mod handle;
#[cfg(feature = "chain")]
pub mod index;
pub mod service;

use std::sync::Arc;

use alloy_chains::NamedChain;
use alloy_primitives::Address;
use alloy_signer::SignerSync;
use tokio::sync::mpsc;
use vertex_swarm_api::{
    Au, SwarmAccountingConfig, SwarmBandwidthAccounting, SwarmError, SwarmPeerState, SwarmResult,
    SwarmSettlementProvider,
};
use vertex_swarm_client_protocol::ClientCommand;
use vertex_swarm_primitives::OverlayAddress;

pub use error::SwapSettlementError;
pub use handle::SwapHandle;
pub use service::{PeerSwapInfo, SwapCommand, SwapService};
pub use vertex_swarm_client_protocol::SwapEvent;

/// Chequebook-based settlement provider.
///
/// On `settle()`, when the peer's debt crosses the payment threshold the
/// provider delegates to the service, which issues and sends a signed cheque.
/// Without a handle it is inert (it never issues cheques on its own), so it
/// composes safely alongside pseudosettle.
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

    /// The early-payment trigger: pay once debt reaches this fraction of the
    /// payment threshold, mirroring pseudosettle's early-payment behaviour.
    fn early_payment_trigger(&self) -> Au {
        let threshold = self.config.payment_threshold();
        let early = self.config.early_payment_percent().min(100);
        let scaled = threshold
            .checked_scale(100 - early)
            .unwrap_or(Au::from_amount(u64::MAX));
        Au::from_amount(scaled.as_amount() / 100)
    }
}

#[async_trait::async_trait]
impl<C: SwarmAccountingConfig + 'static> SwarmSettlementProvider for SwapProvider<C> {
    fn pre_allow(&self, _peer: OverlayAddress, _state: &dyn SwarmPeerState) -> Au {
        // SWAP does not modify the balance during the allow check; payment is
        // driven by `settle()` once debt crosses the threshold.
        Au::ZERO
    }

    async fn settle(&self, peer: OverlayAddress, state: &dyn SwarmPeerState) -> SwarmResult<Au> {
        let balance = state.balance();
        // Positive balance means the peer owes us; nothing for us to pay.
        if !balance.is_negative() {
            return Ok(Au::ZERO);
        }

        let debt = balance.unsigned_abs();
        // Only pay once debt reaches the early-payment trigger.
        if debt < self.early_payment_trigger() {
            return Ok(Au::ZERO);
        }

        let Some(handle) = &self.handle else {
            // Without a service handle the provider cannot issue cheques.
            return Ok(Au::ZERO);
        };

        let accepted = handle
            .settle(peer, debt)
            .await
            .map_err(SwarmError::payment_required)?;

        Ok(accepted)
    }

    fn name(&self) -> &'static str {
        "swap"
    }
}

/// Create a swap actor (service + handle pair).
///
/// Spawn the service as a background task. Use the handle to create a
/// [`SwapProvider`]. The `signer` signs issued cheques, `chequebook` is this
/// node's chequebook (the drawer), `beneficiary` is our payout address (the only
/// address a cheque sent to us may name), and `chain` binds the EIP-712 domain to
/// the settlement chain.
pub fn create_swap_actor<A, S>(
    event_rx: mpsc::UnboundedReceiver<SwapEvent>,
    client_command_tx: mpsc::UnboundedSender<ClientCommand>,
    accounting: Arc<A>,
    signer: Arc<S>,
    chequebook: Address,
    beneficiary: Address,
    chain: NamedChain,
) -> (SwapService<A, S>, SwapHandle)
where
    A: SwarmBandwidthAccounting + 'static,
    S: SignerSync + Send + Sync + 'static,
{
    let (command_tx, command_rx) = mpsc::unbounded_channel();

    let service = SwapService::new(
        command_rx,
        event_rx,
        client_command_tx,
        accounting,
        signer,
        chequebook,
        beneficiary,
        chain,
    );

    let handle = SwapHandle::new(command_tx);

    (service, handle)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, U256};
    use alloy_signer_local::PrivateKeySigner;
    use vertex_swarm_accounting_chequebook::{Cheque, ChequeExt, SignedCheque};
    use vertex_swarm_api::SwarmAccountingConfig;

    const CHAIN: NamedChain = NamedChain::Gnosis;

    struct SwapTestConfig;

    impl SwarmAccountingConfig for SwapTestConfig {
        fn payment_threshold(&self) -> Au {
            Au::from_amount(13_500_000)
        }

        fn payment_tolerance_percent(&self) -> u64 {
            25
        }

        fn refresh_rate(&self) -> Au {
            Au::from_amount(4_500_000)
        }

        fn early_payment_percent(&self) -> u64 {
            50
        }

        fn client_only_factor(&self) -> u64 {
            10
        }
    }

    #[test]
    fn provider_name() {
        let provider = SwapProvider::new(SwapTestConfig);
        assert_eq!(provider.name(), "swap");
    }

    #[test]
    fn early_payment_trigger_is_fraction_of_threshold() {
        let provider = SwapProvider::new(SwapTestConfig);
        // 50% early payment => trigger at 50% of the 13_500_000 threshold.
        assert_eq!(provider.early_payment_trigger(), Au::from_amount(6_750_000));
    }

    /// A signed cheque must recover to the signer that produced it.
    #[test]
    fn cheque_sign_recover_roundtrip() {
        let signer = PrivateKeySigner::random();
        let cheque = Cheque::new(
            Address::repeat_byte(0x11),
            Address::repeat_byte(0x22),
            U256::from(1_000u64),
        );
        let hash = cheque.signing_hash(CHAIN);
        let sig = signer.sign_hash_sync(&hash).unwrap();
        let signed = SignedCheque::from_signature(cheque, sig);

        assert_eq!(signed.recover_signer(CHAIN).unwrap(), signer.address());
    }
}
