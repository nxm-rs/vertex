//! Default constants for SWAP settlement.

use alloy_primitives::U256;

/// Default per-peer uncashed cheque exposure cap, ten times the payment
/// threshold. Bounds free service while on-chain cashing is stubbed.
pub const DEFAULT_BOUNCE_LIMIT: U256 = U256::from_limbs([135_000_000, 0, 0, 0]);
