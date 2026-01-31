//! Pseudosettle handle for interacting with the service.
//!
//! The handle is cheap to clone and can be used from multiple tasks.

use tokio::sync::{mpsc, oneshot};
use vertex_primitives::OverlayAddress;

use crate::error::PseudosettleError;
use crate::service::PseudosettleCommand;

/// Handle for interacting with the pseudosettle service.
///
/// This handle is cheap to clone and can be used from multiple tasks
/// to request settlements. Each settlement request returns a future
/// that resolves when the peer acknowledges (or rejects) the settlement.
#[derive(Clone)]
pub struct PseudosettleHandle {
    command_tx: mpsc::UnboundedSender<PseudosettleCommand>,
}

impl PseudosettleHandle {
    /// Create a new handle from a command sender.
    pub fn new(command_tx: mpsc::UnboundedSender<PseudosettleCommand>) -> Self {
        Self { command_tx }
    }

    /// Request settlement with a peer.
    ///
    /// Returns the amount actually accepted by the peer. This may be less
    /// than the requested amount if the peer's time-based allowance is
    /// insufficient.
    ///
    /// # Errors
    ///
    /// - [`PseudosettleError::ServiceStopped`] if the service has stopped
    /// - [`PseudosettleError::SettlementInProgress`] if there's already a pending settlement
    /// - [`PseudosettleError::TooSoon`] if rate limited
    /// - [`PseudosettleError::NetworkError`] if the network request failed
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
