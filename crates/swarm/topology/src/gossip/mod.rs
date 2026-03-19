//! Gossip coordination for peer discovery and verification.

mod error;
mod events;
mod filter;
mod task;
mod verifier;

use tokio::sync::mpsc;

pub(crate) use events::{GossipAction, GossipInput};
pub(crate) use task::spawn_gossip_task;

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
