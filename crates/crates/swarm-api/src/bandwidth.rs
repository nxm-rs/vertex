//! Bandwidth management traits

use crate::Result;
use vertex_primitives::{Direction, PeerId};

#[cfg(not(feature = "std"))]
use alloc::string::String;
use core::{fmt::Debug, time::Duration};

/// Configuration for pseudosettle (free bandwidth allocation)
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PseudoSettleConfig {
    /// Daily free bandwidth allowance in bytes
    pub daily_allowance: u64,
    /// Payment threshold in bytes
    pub payment_threshold: u64,
    /// Payment tolerance before disconnection in bytes
    pub payment_tolerance: u64,
    /// Disconnect threshold in bytes
    pub disconnect_threshold: u64,
}

impl Default for PseudoSettleConfig {
    fn default() -> Self {
        Self {
            daily_allowance: 1_000_000, // 1MB per day free
            payment_threshold: 10_000_000, // 10MB
            payment_tolerance: 5_000_000, // 5MB
            disconnect_threshold: 50_000_000, // 50MB
        }
    }
}

/// Configuration for SWAP payment channel based bandwidth accounting
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SwapConfig {
    /// Minimum deposit in base units
    pub min_deposit: u64,
    /// Minimum settlement threshold in base units
    pub min_settlement: u64,
    /// Settlement timeout in seconds
    pub settlement_timeout: u64,
    /// Whether to enforce timeouts
    pub enforce_timeouts: bool,
    /// Price per byte in base units
    pub price_per_byte: u64,
}

impl Default for SwapConfig {
    fn default() -> Self {
        Self {
            min_deposit: 1000,  // 1000 base units
            min_settlement: 100, // 100 base units
            settlement_timeout: 86400, // 24 hours
            enforce_timeouts: true,
            price_per_byte: 10, // 10 base units per byte
        }
    }
}

/// Bandwidth accounting for a peer
#[auto_impl::auto_impl(&, Arc)]
pub trait BandwidthAccountant: Send + Sync + 'static {
    /// Record bandwidth usage with a peer
    fn record_usage(&self, peer: &PeerId, bytes: u64, direction: Direction) -> Result<()>;

    /// Get current balance with a peer (positive = they owe us, negative = we owe them)
    fn balance(&self, peer: &PeerId) -> Result<i64>;

    /// Check if a peer has exceeded their debt limit
    fn has_exceeded_limit(&self, peer: &PeerId) -> Result<bool>;

    /// Reset balances for all peers (e.g., at the start of a new period)
    fn reset_balances(&self) -> Result<()>;
}

/// Status of payments with a peer
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PaymentStatus {
    /// Current balance in base units
    pub balance: i64,
    /// Last payment timestamp
    pub last_payment_time: u64,
    /// Last settlement timestamp
    pub last_settlement_time: u64,
    /// Whether payment channel is established
    pub channel_established: bool,
}

/// Current bandwidth status with a peer
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct BandwidthStatus {
    /// Current balance in bytes (positive = they owe us, negative = we owe them)
    pub balance_bytes: i64,
    /// Current balance in token base units
    pub balance_tokens: i64,
    /// Free bandwidth remaining for this period
    pub free_allowance_remaining: u64,
    /// Whether the peer has exceeded limits
    pub exceeds_limit: bool,
    /// Payment information
    pub payment_info: Option<PaymentStatus>,
}

/// Bandwidth payment management
#[async_trait]
#[auto_impl::auto_impl(&, Arc)]
pub trait BandwidthPaymentManager: Send + Sync + 'static {
    /// Settle debt with a peer
    async fn settle(&self, peer: &PeerId) -> Result<()>;

    /// Process an incoming payment from a peer
    fn process_payment(&self, peer: &PeerId, amount: u64, payment_data: &[u8]) -> Result<()>;

    /// Get payment status for a peer
    fn payment_status(&self, peer: &PeerId) -> Result<PaymentStatus>;
}

/// Combined bandwidth controller for accounting and payments
#[async_trait]
#[auto_impl::auto_impl(&, Arc)]
pub trait BandwidthController: Send + Sync + 'static {
    /// Record bandwidth usage
    fn record_usage(&self, peer: &PeerId, bytes: u64, direction: Direction) -> Result<()>;

    /// Check if a peer is allowed to use more bandwidth
    fn allow_bandwidth(&self, peer: &PeerId, bytes: u64) -> Result<bool>;

    /// Settle payments with a peer
    async fn settle(&self, peer: &PeerId) -> Result<()>;

    /// Get bandwidth status for a peer
    fn bandwidth_status(&self, peer: &PeerId) -> Result<BandwidthStatus>;

    /// Get the price per byte for bandwidth
    fn price_per_byte(&self) -> u64;
}
