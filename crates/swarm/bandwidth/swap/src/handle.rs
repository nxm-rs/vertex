//! Swap handle for interacting with the service.
//!
//! The handle is cheap to clone and can be used from multiple tasks.

use tokio::sync::{mpsc, oneshot};
use vertex_swarm_primitives::OverlayAddress;

use crate::error::SwapError;
use crate::service::SwapCommand;

/// Handle for interacting with the swap service.
///
/// This handle is cheap to clone and can be used from multiple tasks
/// to request settlements. Each settlement request returns a future
/// that resolves when the cheque is sent and acknowledged.
#[derive(Clone)]
pub struct SwapHandle {
    command_tx: mpsc::UnboundedSender<SwapCommand>,
}

impl SwapHandle {
    /// Create a new handle from a command sender.
    pub fn new(command_tx: mpsc::UnboundedSender<SwapCommand>) -> Self {
        Self { command_tx }
    }

    /// Request settlement with a peer via cheque.
    ///
    /// Returns the amount actually settled.
    ///
    /// # Errors
    ///
    /// - [`SwapError::ServiceStopped`] if the service has stopped
    /// - [`SwapError::SettlementInProgress`] if there's already a pending settlement
    /// - [`SwapError::SigningFailed`] if cheque signing fails
    /// - [`SwapError::InsufficientBalance`] if chequebook has insufficient balance
    pub async fn settle(
        &self,
        peer: OverlayAddress,
        amount: u64,
    ) -> Result<u64, SwapError> {
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
