//! Gossip coordination for peer discovery and verification.
//!
//! Peers learned through gossip are untrusted until a verification handshake
//! confirms them, so the subsystem is bounded at every stage. All bounds are
//! collected in [`GossipConfig`]; its defaults are the production tuning and
//! tests tighten individual fields for deterministic timing.
//!
//! The limits relate to each other in layers:
//!
//! - Admission: `max_pending_per_gossiper` caps what one source can queue;
//!   `max_total_pending` caps the queue overall. The global cap is sized for
//!   broad load from many gossipers (roughly 32 sources sending about 30
//!   peers each), so it binds before the per-source caps could in aggregate.
//!   `max_tracked_gossipers` bounds the rate-limit bookkeeping itself and
//!   must cover the expected number of concurrent gossipers for the
//!   per-source cap to hold.
//! - Draining: `max_concurrent_verifications` bounds simultaneous
//!   verification dials, and `pending_expiry` (swept every
//!   `cleanup_interval`) evicts entries that never get dialed, so a full
//!   queue always recovers.
//! - Failure damping: unreachable peers back off exponentially from
//!   `backoff_base` to `backoff_max`, and after `ban_after_failures`
//!   consecutive failures they are banned for `ban_ttl`. The caches behind
//!   these are LRU-bounded by `backoff_capacity` and `ban_capacity`.
//! - Exchange cadence: `refresh_interval` paces neighborhood broadcasts and
//!   `health_check_delay` defers exchanges on fresh gossip dials until the
//!   connection proves stable.

mod config;
mod error;
mod events;
mod filter;
mod tasks;
mod verifier;

use tokio::sync::mpsc;

pub use config::GossipConfig;
pub(crate) use events::{GossipAction, GossipInput};
pub(crate) use tasks::{GossipChannels, gossip_channel, spawn_gossip_task};

/// Handle for communicating with the gossip task.
pub(crate) struct GossipHandle {
    input_tx: mpsc::Sender<GossipInput>,
    output_rx: mpsc::Receiver<GossipAction>,
}

impl GossipHandle {
    /// Send an input event to the gossip task.
    pub(crate) fn send(&self, input: GossipInput) {
        if let Err(e) = self.input_tx.try_send(input) {
            tracing::warn!("Gossip input channel full, dropping event: {e}");
        }
    }

    /// Try to receive a gossip broadcast action (non-blocking).
    pub(crate) fn try_recv(&mut self) -> Result<GossipAction, mpsc::error::TryRecvError> {
        self.output_rx.try_recv()
    }
}
