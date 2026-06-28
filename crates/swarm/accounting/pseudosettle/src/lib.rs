//! Time-based settlement provider for bandwidth accounting.
//!
//! Peers forgive a time-based allowance (refresh rate times elapsed seconds)
//! when they receive a settle from us. A debtor never forgives its own debt
//! locally: our balance drops only when the peer acks a settle we sent, so the
//! debt we track matches the debt the peer records.
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
pub use service::{PseudosettleCommand, PseudosettleService};
pub use vertex_swarm_client_protocol::PseudosettleEvent;

/// Debtor-initiated time-based settlement provider.
///
/// `settle()` sends a pseudosettle to the peer offering our debt; the peer
/// forgives up to its time-based allowance and acks the accepted amount, which
/// the service credits back. `pre_allow()` is a no-op: a debtor must not forgive
/// its own debt on elapsed time, or our balance would understate the debt the
/// peer records and we would under-settle.
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
    fn pre_allow(&self, _peer: OverlayAddress, _state: &dyn SwarmPeerState) -> Au {
        // A debtor never forgives its own debt locally. The peer reduces its view
        // of our debt only when it acks a settle we sent; crediting our balance
        // on elapsed time here would mask a debt that has actually climbed at the
        // peer, suppressing the settle that recovers it and letting the peer drop
        // us. Debt recovers only through the network ack credited in the service.
        Au::ZERO
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
            // No handle: nothing to send over the wire, so nothing settles.
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
        // A debtor's balance must track the debt the peer records; only a network
        // ack reduces it. `pre_allow` leaves the balance untouched however much
        // time has elapsed, so the debt cannot look settled locally while the peer
        // still counts it.
        let provider = PseudosettleProvider::new(BandwidthConfig::default());
        let state = PeerState::new(Au::from_amount(13_500_000), Au::from_amount(16_875_000));
        state.add_balance(Au::new(-10_000));
        state.set_last_refresh(vertex_util_runtime::time::now_unix_secs() - 10);

        let credit = provider.pre_allow(test_peer(), &state);

        assert_eq!(credit, Au::ZERO);
        assert_eq!(state.balance(), Au::new(-10_000));
    }

    #[tokio::test]
    async fn settle_without_handle_is_a_noop() {
        // A handle-less provider has no wire to send a pseudosettle on, so settle
        // is a no-op even with an outstanding debt.
        let provider = PseudosettleProvider::new(BandwidthConfig::default());
        let state = PeerState::new(Au::from_amount(13_500_000), Au::from_amount(16_875_000));
        state.add_balance(Au::new(-10_000));

        let settled = provider
            .settle(test_peer(), &state)
            .await
            .expect("handle-less settle is a no-op");
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
}
