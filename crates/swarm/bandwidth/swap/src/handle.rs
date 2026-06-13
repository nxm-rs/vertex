//! Cloneable handle for interacting with the swap service.

use tokio::sync::{mpsc, oneshot};
use vertex_swarm_api::Au;
use vertex_swarm_primitives::OverlayAddress;

use crate::error::SwapSettlementError;
use crate::service::{PeerSwapInfo, SwapCommand};

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

    /// Request cheque settlement. Returns the amount settled in AU.
    pub async fn settle(
        &self,
        peer: OverlayAddress,
        amount: Au,
    ) -> Result<Au, SwapSettlementError> {
        let (tx, rx) = oneshot::channel();

        self.command_tx
            .send(SwapCommand::Settle {
                peer,
                amount,
                response_tx: tx,
            })
            .map_err(|_| SwapSettlementError::ServiceStopped)?;

        rx.await.map_err(|_| SwapSettlementError::ServiceStopped)?
    }

    /// Register the SWAP identity (beneficiary and chequebook issuer) learned for
    /// a peer during the swap handshake.
    pub fn register_peer_info(
        &self,
        peer: OverlayAddress,
        info: PeerSwapInfo,
    ) -> Result<(), SwapSettlementError> {
        self.command_tx
            .send(SwapCommand::RegisterPeerInfo { peer, info })
            .map_err(|_| SwapSettlementError::ServiceStopped)
    }
}
