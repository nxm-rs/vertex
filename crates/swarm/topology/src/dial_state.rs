//! Read-only snapshot of the dialer's decision state.

use std::time::Duration;

use vertex_swarm_primitives::Bin;

/// Point-in-time view of the dial machinery, read from the authoritative
/// tables, tracker, manager, and GCRA bucket, never from metrics.
///
/// Assembled inside the behaviour poll so behaviour-owned state (the dial
/// tracker and the dial-rate bucket) is read in a single pass and cannot
/// tear against a concurrent poll.
#[derive(Debug, Clone)]
pub struct DialState {
    /// Dials the tracker has dispatched and is awaiting an outcome for.
    pub in_flight_dials: usize,
    /// Dials the tracker holds queued behind the in-flight cap.
    pub pending_dials: usize,
    /// Candidates queued across all bins awaiting a dial token.
    pub queued_candidates: usize,
    /// Known peers that are not banned. Backoff is not subtracted: a peer
    /// serving a dial-backoff cooldown still counts here and also appears in
    /// [`Self::peers_in_backoff`].
    pub eligible_known_peers: usize,
    /// Known peers currently serving a dial-backoff cooldown.
    pub peers_in_backoff: usize,
    /// Tokens the dial-rate bucket would admit right now.
    pub available_dial_tokens: u32,
    /// Wait until the next dial token replenishes, `None` when one is ready.
    pub next_dial_token_in: Option<Duration>,
    /// Whether the dial-rate bucket would refuse a dial right now.
    pub throttled: bool,
    /// Time since the last connection-evaluation round, `None` before the
    /// first.
    pub last_evaluation: Option<Duration>,
    /// Minimum self-dialed peers a finite-target bin holds (clamped read).
    pub min_outbound: usize,
    /// Per-bin dial-state coverage, one entry per tracked bin.
    pub bins: Vec<DialBinState>,
}

/// Per-bin slice of [`DialState`].
#[derive(Debug, Clone)]
pub struct DialBinState {
    /// Proximity-order bin this slice describes.
    pub bin: Bin,
    /// Connected peers in the bin.
    pub connected: usize,
    /// Outstanding dials to the bin (dialing phase).
    pub dialing: usize,
    /// Self-dialed (outbound) connections in the bin.
    pub outbound: usize,
    /// Distinct sub-prefix slots the connected peers cover, `None` for
    /// neighborhood bins which connect to every peer and are not
    /// slot-balanced.
    pub slots_filled: Option<usize>,
    /// Candidates queued for the bin awaiting a dial token.
    pub queued: usize,
}
