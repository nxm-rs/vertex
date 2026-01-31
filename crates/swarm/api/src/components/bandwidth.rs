//! Bandwidth accounting - per-peer balance tracking.
//!
//! Two-level design: [`SwarmBandwidthAccounting`] creates per-peer [`SwarmPeerBandwidth`] handles.
//! Uses overlay addresses for peer identification (not libp2p `PeerId`).
//!
//! # Settlement Providers
//!
//! Settlement is handled by pluggable [`SwarmSettlementProvider`] implementations:
//! - **Pseudosettle**: Time-based debt forgiveness (soft accounting)
//! - **Swap**: Chequebook-based real payments
//!
//! Providers are configured via [`SwarmAccountingConfig`] which specifies the [`BandwidthMode`].

use std::vec::Vec;

use nectar_primitives::ChunkAddress;
use vertex_swarm_primitives::OverlayAddress;

use crate::{SwarmIdentity, SwarmPricing, SwarmResult};

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
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmPeerState: Send + Sync {
    /// Get the current balance (positive = peer owes us).
    fn balance(&self) -> i64;

    /// Add to the balance atomically.
    fn add_balance(&self, amount: i64);

    /// Get the last refresh timestamp (for pseudosettle).
    fn last_refresh(&self) -> u64;

    /// Set the last refresh timestamp.
    fn set_last_refresh(&self, timestamp: u64);

    /// Get the payment threshold for this peer.
    fn payment_threshold(&self) -> u64;

    /// Get the disconnect threshold for this peer.
    fn disconnect_threshold(&self) -> u64;
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
    fn pre_allow(&self, peer: OverlayAddress, state: &dyn SwarmPeerState) -> i64;

    /// Called when explicit settlement is requested.
    /// Returns the amount settled, or an error if settlement failed.
    async fn settle(&self, peer: OverlayAddress, state: &dyn SwarmPeerState) -> SwarmResult<i64>;

    /// Human-readable name for logging.
    fn name(&self) -> &'static str;
}

/// Configuration for bandwidth accounting.
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmAccountingConfig: Send + Sync {
    /// The bandwidth accounting mode.
    fn mode(&self) -> BandwidthMode;

    /// Check if a settlement provider is enabled for this configuration.
    fn provider_enabled(&self, provider: &dyn SwarmSettlementProvider) -> bool {
        let mode = self.mode();
        let supported = provider.supported_mode();
        match supported {
            BandwidthMode::Pseudosettle => mode.pseudosettle_enabled(),
            BandwidthMode::Swap => mode.swap_enabled(),
            BandwidthMode::Both => mode.pseudosettle_enabled() || mode.swap_enabled(),
            BandwidthMode::None => true, // No-op provider always works
        }
    }

    /// Payment threshold (triggers settlement when peer debt exceeds this).
    fn payment_threshold(&self) -> u64;

    /// Payment tolerance percent (disconnect threshold = payment_threshold * (100 + tolerance) / 100).
    fn payment_tolerance_percent(&self) -> u64;

    /// Refresh rate per second (for pseudosettle).
    fn refresh_rate(&self) -> u64;

    /// Early payment trigger percent (for swap).
    fn early_payment_percent(&self) -> u64;

    /// Scaling factor for client-only nodes (divides thresholds).
    fn client_only_factor(&self) -> u64;

    /// Calculate the disconnect threshold.
    fn disconnect_threshold(&self) -> u64 {
        self.payment_threshold() * (100 + self.payment_tolerance_percent()) / 100
    }

    /// Check if bandwidth accounting is enabled.
    fn is_enabled(&self) -> bool {
        self.mode().is_enabled()
    }
}

/// Per-peer bandwidth accounting handle. Clone-safe and lock-free.
#[async_trait::async_trait]
pub trait SwarmPeerBandwidth: Clone + Send + Sync {
    /// Record bandwidth usage (lock-free, must not block).
    fn record(&self, bytes: u64, direction: Direction);

    /// Check if a transfer is allowed (false if over disconnect threshold).
    fn allow(&self, bytes: u64) -> bool;

    /// Get current balance (positive = peer owes us).
    fn balance(&self) -> i64;

    /// Request settlement (may involve network I/O).
    async fn settle(&self) -> SwarmResult<()>;

    /// Get the peer's overlay address.
    fn peer(&self) -> OverlayAddress;
}

/// Factory for creating per-peer bandwidth accounting handles.
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmBandwidthAccounting: Send + Sync {
    /// The node identity type.
    type Identity: SwarmIdentity;

    /// The per-peer accounting handle type.
    type Peer: SwarmPeerBandwidth;

    /// Action for receiving service (balance decreases).
    type ReceiveAction: Send;

    /// Action for providing service (balance increases).
    type ProvideAction: Send;

    /// Get the node's identity.
    fn identity(&self) -> &Self::Identity;

    /// Get or create an accounting handle for a peer.
    fn for_peer(&self, peer: OverlayAddress) -> Self::Peer;

    /// List all peers with active accounting.
    fn peers(&self) -> Vec<OverlayAddress>;

    /// Remove accounting for a peer.
    fn remove_peer(&self, peer: &OverlayAddress);

    /// Prepare to receive service from a peer (balance decreases).
    ///
    /// Returns an action that reserves balance. Call `apply()` to commit
    /// or drop to release the reservation.
    fn prepare_receive(
        &self,
        peer: OverlayAddress,
        price: u64,
        originated: bool,
    ) -> SwarmResult<Self::ReceiveAction>;

    /// Prepare to provide service to a peer (balance increases).
    fn prepare_provide(
        &self,
        peer: OverlayAddress,
        price: u64,
    ) -> SwarmResult<Self::ProvideAction>;
}

/// Combined pricing and bandwidth accounting for client operations.
///
/// Unifies chunk pricing and bandwidth accounting so callers don't need
/// to coordinate between separate pricer and accounting instances.
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmClientAccounting: Clone + Send + Sync {
    /// The underlying bandwidth accounting type.
    type Bandwidth: SwarmBandwidthAccounting;

    /// The pricing strategy type.
    type Pricing: SwarmPricing;

    /// Get the bandwidth accounting.
    fn bandwidth(&self) -> &Self::Bandwidth;

    /// Get the pricer.
    fn pricing(&self) -> &Self::Pricing;

    /// Get the node's identity.
    fn identity(&self) -> &<Self::Bandwidth as SwarmBandwidthAccounting>::Identity {
        self.bandwidth().identity()
    }

    /// Get or create accounting for a peer.
    fn for_peer(&self, peer: OverlayAddress) -> <Self::Bandwidth as SwarmBandwidthAccounting>::Peer {
        self.bandwidth().for_peer(peer)
    }

    /// Prepare to receive a chunk (we pay, balance decreases).
    fn prepare_receive_chunk(
        &self,
        peer: OverlayAddress,
        chunk: &ChunkAddress,
        originated: bool,
    ) -> SwarmResult<<Self::Bandwidth as SwarmBandwidthAccounting>::ReceiveAction> {
        let price = self.pricing().peer_price(&peer, chunk);
        self.bandwidth().prepare_receive(peer, price, originated)
    }

    /// Prepare to provide a chunk (peer pays, balance increases).
    fn prepare_provide_chunk(
        &self,
        peer: OverlayAddress,
        chunk: &ChunkAddress,
    ) -> SwarmResult<<Self::Bandwidth as SwarmBandwidthAccounting>::ProvideAction> {
        let price = self.pricing().peer_price(&peer, chunk);
        self.bandwidth().prepare_provide(peer, price)
    }

    /// Calculate price for receiving a chunk from a peer.
    fn receive_price(&self, peer: &OverlayAddress, chunk: &ChunkAddress) -> u64 {
        self.pricing().peer_price(peer, chunk)
    }

    /// Calculate price for providing a chunk to a peer.
    fn provide_price(&self, peer: &OverlayAddress, chunk: &ChunkAddress) -> u64 {
        self.pricing().peer_price(peer, chunk)
    }
}
