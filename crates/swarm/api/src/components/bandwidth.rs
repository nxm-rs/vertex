//! Bandwidth accounting - per-peer balance tracking.
//!
//! Two-level design: [`SwarmBandwidthAccounting`] creates per-peer [`SwarmPeerBandwidth`] handles.
//! Uses overlay addresses for peer identification (not libp2p `PeerId`).
//!
//! # Settlement Providers
//!
//! Settlement is handled by pluggable [`SwarmSettlementProvider`] implementations:
//! - **Pseudosettle**: Time-based debt forgiveness (soft accounting)
//!
//! Providers are configured via [`SwarmAccountingConfig`] which specifies the [`BandwidthMode`].

use vertex_swarm_primitives::OverlayAddress;

use super::peers::SwarmPeerRegistry;
use crate::SwarmResult;

pub use vertex_swarm_primitives::BandwidthMode;

/// Direction of data transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Uploading data (sending to peer)
    Upload,
    /// Downloading data (receiving from peer)
    Download,
}

/// Abstract peer balance state for settlement providers.
///
/// Positive balance = peer owes us, negative = we owe peer.
/// The peer address is passed separately to settlement methods.
///
/// Trait methods express domain operations, not raw state mutations.
/// Override [`record_refresh`](SwarmPeerAccounting::record_refresh) to
/// customise how refresh timestamps are stored.
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmPeerAccounting: Send + Sync {
    /// Get the current balance (positive = peer owes us).
    fn balance(&self) -> i64;

    /// Add to the balance.
    fn add_balance(&self, amount: i64);

    /// Get the last refresh timestamp (epoch seconds).
    fn last_refresh(&self) -> u64;

    /// Record that a refresh occurred at the given timestamp.
    ///
    /// Called by the default [`apply_refresh`](SwarmPeerAccounting::apply_refresh)
    /// implementation. Override to add bookkeeping (metrics, persistence) when
    /// a refresh is committed.
    fn record_refresh(&self, timestamp: u64);

    /// Get the credit limit for this peer.
    fn credit_limit(&self) -> u64;

    /// Get the disconnect limit for this peer.
    fn disconnect_limit(&self) -> u64;

    /// Apply time-based refresh and return the credit applied.
    ///
    /// If the peer has negative balance (we owe them), credits up to
    /// `elapsed_seconds * refresh_rate`, capped at the absolute debt.
    /// Returns the credit applied (zero if balance is non-negative or
    /// no time has elapsed).
    ///
    /// `now` is the current timestamp in epoch seconds. Passing it
    /// explicitly keeps the trait pure and simplifies testing.
    ///
    /// The default implementation calls [`record_refresh`](SwarmPeerAccounting::record_refresh)
    /// to commit the new timestamp. Override `apply_refresh` itself to change
    /// the credit algorithm entirely.
    fn apply_refresh(&self, now: u64, refresh_rate: u64) -> i64 {
        let last = self.last_refresh();

        if last == 0 {
            self.record_refresh(now);
            return 0;
        }

        let elapsed = now.saturating_sub(last);
        if elapsed == 0 {
            return 0;
        }

        let allowance = elapsed * refresh_rate;

        let balance = self.balance();
        let credit = if balance < 0 {
            let credit = (allowance as i64).min(-balance);
            self.add_balance(credit);
            credit
        } else {
            0
        };

        self.record_refresh(now);
        credit
    }
}

/// Settlement provider for bandwidth accounting.
///
/// Providers are called at `pre_allow()` (before allowing transfers) and
/// `settle()` (when explicit settlement is requested).
#[async_trait::async_trait]
pub trait SwarmSettlementProvider: Send + Sync + 'static {
    /// The bandwidth mode this provider supports.
    fn supported_mode(&self) -> BandwidthMode;

    /// Called before checking if a transfer is allowed.
    /// Returns the amount of balance adjustment (positive = credit added).
    fn pre_allow(&self, peer: OverlayAddress, state: &dyn SwarmPeerAccounting) -> i64;

    /// Called when explicit settlement is requested.
    /// Returns the amount settled, or an error if settlement failed.
    async fn settle(
        &self,
        peer: OverlayAddress,
        state: &dyn SwarmPeerAccounting,
    ) -> SwarmResult<i64>;

    /// Human-readable name for logging.
    fn name(&self) -> &'static str;
}

/// Configuration for bandwidth accounting.
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmAccountingConfig: Send + Sync {
    /// The bandwidth accounting mode.
    fn mode(&self) -> BandwidthMode;

    /// Credit limit (triggers settlement when peer debt exceeds this).
    fn credit_limit(&self) -> u64;

    /// Credit tolerance percent (disconnect limit = credit_limit * (100 + tolerance) / 100).
    fn credit_tolerance_percent(&self) -> u64;

    /// Refresh rate per second (for pseudosettle).
    fn refresh_rate(&self) -> u64;

    /// Early payment trigger percent (for swap).
    fn early_payment_percent(&self) -> u64;

    /// Scaling factor for client-only nodes (divides thresholds).
    fn client_only_factor(&self) -> u64;

    /// Calculate the disconnect limit.
    fn disconnect_limit(&self) -> u64 {
        self.credit_limit() * (100 + self.credit_tolerance_percent()) / 100
    }

    /// Check if bandwidth accounting is enabled.
    fn is_enabled(&self) -> bool {
        self.mode().is_enabled()
    }
}

/// Per-peer bandwidth accounting handle.
#[async_trait::async_trait]
pub trait SwarmPeerBandwidth: Send + Sync {
    /// Record a chunk transfer in accounting units (must not block).
    fn record(&self, price: u64, direction: Direction);

    /// Check if a transfer of the given price is allowed (false if over disconnect limit).
    fn allow(&self, price: u64) -> bool;

    /// Get current balance (positive = peer owes us).
    fn balance(&self) -> i64;

    /// Request settlement (may involve network I/O).
    async fn settle(&self) -> SwarmResult<()>;

    /// Get the peer's overlay address.
    fn peer(&self) -> OverlayAddress;
}

/// Balance reservation for bandwidth accounting.
///
/// Extends [`SwarmPeerRegistry`](crate::SwarmPeerRegistry) with prepare/apply
/// balance operations. The `Peer` handle must implement [`SwarmPeerBandwidth`].
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmBandwidthAccounting: SwarmPeerRegistry<Peer: SwarmPeerBandwidth> {
    /// Action for balance changes (receive or provide).
    type Action: Send;

    /// Prepare to receive service from a peer (balance decreases).
    ///
    /// Returns an action that reserves balance. Call `apply()` to commit
    /// or drop to release the reservation.
    fn prepare_receive(
        &self,
        peer: OverlayAddress,
        price: u64,
        originated: bool,
    ) -> SwarmResult<Self::Action>;

    /// Prepare to provide service to a peer (balance increases).
    fn prepare_provide(&self, peer: OverlayAddress, price: u64) -> SwarmResult<Self::Action>;
}
