//! Default constants for SWAP settlement.

use alloy_primitives::U256;

/// Default per-peer uncashed cheque exposure cap, in cumulative-payout (wire)
/// units.
///
/// A received cheque reduces a peer's debt on structural validation alone, while
/// on-chain cashing is stubbed for v1, so nothing else bounds how much uncashed
/// value we treat as settled. This caps that exposure to ten times the default
/// payment threshold (`13_500_000` AU, one wire unit per AU), so a counterparty
/// cannot buy unbounded real service with cheques that may never cash. The real
/// fix is to credit only on confirmed cashout; see #438.
pub const DEFAULT_BOUNCE_LIMIT: U256 = U256::from_limbs([135_000_000, 0, 0, 0]);
