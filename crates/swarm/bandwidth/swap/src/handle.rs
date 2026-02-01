//! Cloneable handle for interacting with the swap service.

use tokio::sync::{mpsc, oneshot};
use vertex_swarm_primitives::OverlayAddress;

use crate::error::SwapError;
use crate::service::SwapCommand;

/// Cloneable handle for requesting cheque settlements from the service.
#[derive(Clone)]
pub struct SwapHandle {
    command_tx: mpsc::UnboundedSender<SwapCommand>,
}

impl SwapHandle {
    /// Create a new handle from a command sender.
    pub fn new(command_tx: mpsc::UnboundedSender<SwapCommand>) -> Self {
        Self { command_tx }
    }

    /// Request cheque settlement. Returns the amount settled.
    pub async fn settle(&self, peer: OverlayAddress, amount: u64) -> Result<u64, SwapError> {
        let (tx, rx) = oneshot::channel();

        self.command_tx
            .send(SwapCommand::Settle {
                peer,
                amount,
                response_tx: tx,
            })
            .map_err(|_| SwapError::ServiceStopped)?;

        rx.await.map_err(|_| SwapError::ServiceStopped)?
    }
}
