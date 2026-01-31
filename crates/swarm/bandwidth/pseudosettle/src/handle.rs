//! Cloneable handle for interacting with the pseudosettle service.

use tokio::sync::{mpsc, oneshot};
use vertex_swarm_primitives::OverlayAddress;

use crate::error::PseudosettleError;
use crate::service::PseudosettleCommand;

/// Cloneable handle for requesting settlements from the service.
#[derive(Clone)]
pub struct PseudosettleHandle {
    command_tx: mpsc::UnboundedSender<PseudosettleCommand>,
}

impl PseudosettleHandle {
    /// Create a new handle from a command sender.
    pub fn new(command_tx: mpsc::UnboundedSender<PseudosettleCommand>) -> Self {
        Self { command_tx }
    }

    /// Request settlement. Returns the amount accepted (may be less than requested).
    pub async fn settle(
        &self,
        peer: OverlayAddress,
        amount: u64,
    ) -> Result<u64, PseudosettleError> {
        let (tx, rx) = oneshot::channel();

        self.command_tx
            .send(PseudosettleCommand::Settle {
                peer,
                amount,
                response_tx: tx,
            })
            .map_err(|_| PseudosettleError::ServiceStopped)?;

        rx.await.map_err(|_| PseudosettleError::ServiceStopped)?
    }
}
