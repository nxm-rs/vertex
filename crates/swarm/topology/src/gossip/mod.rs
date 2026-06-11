//! Gossip coordination for peer discovery.
//!
//! Records learned through gossip pass full signature validation at the
//! hive protocol layer, then flow through a bounded intake gate into the
//! peer manager as unverified, dialable entries. Candidate
//! selection may dial them like any other supply; the first completed
//! handshake on a real connection verifies the record in the same round
//! trip, so no separate verification dial or ephemeral identity exists.
//! All bounds are collected in [`GossipConfig`]; its defaults are the
//! production tuning and tests tighten individual fields for deterministic
//! timing.
//!
//! The limits relate to each other in layers:
//!
//! - Intake damping: `record_cooldown` suppresses re-signed records whose
//!   multiaddrs have not changed (peers may re-sign their record on every
//!   broadcast), while changed multiaddrs bypass the cooldown. The same
//!   interval is the window for the per-gossiper budget
//!   `max_records_per_gossiper`, so one source cannot flood the known
//!   table. `max_tracked_gossipers` and `max_tracked_cooldowns` bound the
//!   bookkeeping itself.
//! - Failure damping: admitted records live in the peer manager, so failed
//!   dials use its per-peer backoff, and unverified entries expire on a
//!   short failure budget (see the peer manager's stale policy) instead of
//!   polluting candidate supply.
//! - Exchange cadence: `refresh_interval` paces neighborhood broadcasts and
//!   `health_check_delay` defers exchanges on fresh gossip dials until the
//!   connection proves stable.

mod config;
mod error;
mod events;
mod filter;
mod intake;
mod tasks;

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
