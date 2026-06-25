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
    Au, SwarmAccountingConfig, SwarmBandwidthAccounting, SwarmError, SwarmIdentity, SwarmPeerState,
    SwarmResult, SwarmSettlementProvider,
};
use vertex_swarm_client_protocol::ClientCommand;
use vertex_swarm_primitives::OverlayAddress;

pub use error::PseudosettleSettlementError;
pub use handle::PseudosettleHandle;
pub use service::{PseudosettleCommand, PseudosettleService, pseudosettle_stats};
pub use vertex_swarm_client_protocol::PseudosettleEvent;

/// Time-based settlement provider.
///
/// `settle()` sends a pseudosettle message to the peer offering our real debt;
/// the peer forgives up to its time-based allowance and acks the accepted
/// amount, which the service credits back. Settlement is gated on the
/// early-payment trigger so we offer once debt approaches the payment threshold,
/// before the peer would refuse or drop us.
///
/// The debtor's balance is never forgiven locally: a peer's view of our debt
/// only drops when it acks a pseudosettle we sent, so our balance must track
/// that real debt and recover only on the ack. `pre_allow()` is therefore a
/// no-op; a local time-refresh here would mask the debt and suppress the network
/// settle that actually reduces it at the peer.
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

    /// The early-payment trigger: settle once debt reaches this fraction of the
    /// payment threshold. Mirrors the reference accounting, which refreshes
    /// before debt reaches the threshold so a concurrent burst does not block on
    /// or trip the peer's limit. The trigger is `(100 - early_percent)%` of the
    /// payment threshold, floored at one refresh-rate unit so a settle always
    /// offers at least the minimum the peer will act on.
    fn early_payment_trigger(&self) -> Au {
        let threshold = self.config.payment_threshold();
        let early = self.config.early_payment_percent().min(100);
        let scaled = threshold
            .checked_scale(100 - early)
            .unwrap_or(Au::from_amount(u64::MAX));
        Au::from_amount(scaled.as_amount() / 100).max(self.config.refresh_rate())
    }
}

#[async_trait::async_trait]
impl<C: SwarmAccountingConfig + 'static> SwarmSettlementProvider for PseudosettleProvider<C> {
    fn pre_allow(&self, _peer: OverlayAddress, _state: &dyn SwarmPeerState) -> Au {
        // The debtor never forgives its own debt locally: a peer reduces its view
        // of our debt only when it acks a pseudosettle we sent. Crediting our own
        // balance here would mask the real debt, so a debt that has actually
        // climbed at the peer would look settled locally and `settle()` would
        // never fire, letting the peer cut us off. Debt recovers only through the
        // network ack credited in the service.
        Au::ZERO
    }

    async fn settle(&self, peer: OverlayAddress, state: &dyn SwarmPeerState) -> SwarmResult<Au> {
        let Some(handle) = &self.handle else {
            // Without a service handle the provider cannot send pseudosettle.
            return Ok(Au::ZERO);
        };

        let balance = state.balance();
        // Positive balance means the peer owes us; nothing for us to settle.
        if !balance.is_negative() {
            return Ok(Au::ZERO);
        }

        let debt = balance.unsigned_abs();
        // Only settle once debt reaches the early-payment trigger, so we offer
        // before the peer's limit rather than on every chunk.
        if debt < self.early_payment_trigger() {
            return Ok(Au::ZERO);
        }

        // Offer the whole debt; the peer forgives up to its time-based allowance
        // and acks the accepted amount, which the service credits back.
        let accepted = handle
            .settle(peer, debt)
            .await
            .map_err(SwarmError::payment_required)?;

        Ok(accepted)
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
    fn pre_allow_never_forgives_debt_locally() {
        // The debtor's balance must track the real debt the peer sees; only a
        // network ack reduces it. `pre_allow` must therefore leave the balance
        // untouched, however much time has elapsed, so the debt cannot look
        // settled locally while the peer still counts it.
        let provider = PseudosettleProvider::new(BandwidthConfig::default());
        let state = PeerState::new(Au::from_amount(13_500_000), Au::from_amount(16_875_000));
        state.add_balance(Au::new(-10_000));
        state.set_last_refresh(vertex_util_runtime::time::now_unix_secs() - 10);

        let credit = provider.pre_allow(test_peer(), &state);

        assert_eq!(credit, Au::ZERO);
        assert_eq!(state.balance(), Au::new(-10_000));
    }

    #[test]
    fn early_payment_trigger_is_fraction_of_threshold() {
        // Default config: 13_500_000 threshold, 50% early percent => settle once
        // debt reaches (100-50)% of the threshold, floored at one refresh-rate
        // unit. Here the 50% figure (6_750_000) exceeds the refresh rate, so it
        // wins.
        let provider = PseudosettleProvider::new(BandwidthConfig::default());
        assert_eq!(provider.early_payment_trigger(), Au::from_amount(6_750_000));
    }

    #[tokio::test]
    async fn settle_is_a_noop_below_the_early_payment_trigger() {
        // A small debt must not settle: a settle this far below the threshold
        // would waste a round trip the peer would mostly time-cap to nothing.
        let provider = PseudosettleProvider::new(BandwidthConfig::default());
        let state = PeerState::new(Au::from_amount(13_500_000), Au::from_amount(16_875_000));
        state.add_balance(Au::new(-1_000));

        let settled = provider
            .settle(test_peer(), &state)
            .await
            .expect("settle below trigger is a no-op");
        assert_eq!(settled, Au::ZERO);
    }

    #[tokio::test]
    async fn settle_is_a_noop_when_not_in_debt() {
        // A positive balance means the peer owes us; there is nothing to settle.
        let provider = PseudosettleProvider::new(BandwidthConfig::default());
        let state = PeerState::new(Au::from_amount(13_500_000), Au::from_amount(16_875_000));
        state.add_balance(Au::new(1_000_000));

        let settled = provider
            .settle(test_peer(), &state)
            .await
            .expect("settle with a creditor balance is a no-op");
        assert_eq!(settled, Au::ZERO);
    }

    #[tokio::test]
    async fn settle_without_handle_is_a_noop() {
        // The handle-less provider (no network wiring) cannot send pseudosettle,
        // even with a debt past the trigger.
        let provider = PseudosettleProvider::new(BandwidthConfig::default());
        let state = PeerState::new(Au::from_amount(13_500_000), Au::from_amount(16_875_000));
        state.add_balance(Au::new(-10_000_000));

        let settled = provider
            .settle(test_peer(), &state)
            .await
            .expect("handle-less settle is a no-op");
        assert_eq!(settled, Au::ZERO);
    }

    #[tokio::test]
    async fn settle_sends_full_debt_once_over_the_trigger() {
        // With a debt past the early-payment trigger and a wired handle, settle
        // offers the whole debt on the wire. The drained command channel returns
        // a service-stopped error mapped to payment_required, which proves the
        // offer was sent (and carried the full debt) rather than skipped.
        let (command_tx, mut command_rx) = mpsc::unbounded_channel();
        let provider = PseudosettleProvider::with_handle(
            BandwidthConfig::default(),
            PseudosettleHandle::new(command_tx),
        );
        let state = PeerState::new(Au::from_amount(13_500_000), Au::from_amount(16_875_000));
        let debt = 10_000_000;
        state.add_balance(Au::new(-debt));

        let peer = test_peer();
        let settle = tokio::spawn(async move { provider.settle(peer, &state).await });

        // The provider sent a settle command carrying the full debt.
        let cmd = command_rx.recv().await.expect("settle command sent");
        match cmd {
            PseudosettleCommand::Settle {
                peer: p,
                amount,
                response_tx,
            } => {
                assert_eq!(p, peer);
                assert_eq!(amount, Au::from_amount(debt as u64));
                // Drop the response channel so the provider's pending await
                // resolves with a cancellation; the offer on the wire was the
                // observable effect. Holding it (via `..`) would deadlock the
                // awaited settle below.
                drop(response_tx);
            }
        }
        let _ = settle.await;
    }
}
