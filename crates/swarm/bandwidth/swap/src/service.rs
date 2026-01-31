//! Swap service actor.
//!
//! This module implements the Handle+Service actor pattern for swap settlement.
//! The service runs in its own tokio task and processes settlement commands
//! and network events.

use std::collections::HashMap;
use std::sync::Arc;

use alloy_primitives::U256;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};
use vertex_swarm_client::{protocol::ClientCommand, SwapEvent};
use vertex_swarm_primitives::OverlayAddress;
use vertex_swarm_api::{SwarmBandwidthAccounting, Direction, SwarmPeerBandwidth};

use crate::error::SwapError;

/// Commands from the handle to the service.
pub enum SwapCommand {
    /// Request settlement with a peer.
    Settle {
        /// The peer to settle with.
        peer: OverlayAddress,
        /// The amount to settle.
        amount: u64,
        /// Channel to send the result.
        response_tx: oneshot::Sender<Result<u64, SwapError>>,
    },
}

/// The swap service runs in its own tokio task.
///
/// It processes:
/// - Settlement commands from handles (outbound cheque issuance)
/// - Network events (inbound cheques and acks)
///
/// # Generic Parameters
///
/// - `A`: The accounting implementation (must implement `SwarmBandwidthAccounting`)
pub struct SwapService<A: SwarmBandwidthAccounting> {
    /// Receive commands from handles.
    command_rx: mpsc::UnboundedReceiver<SwapCommand>,
    /// Receive events routed from the network layer.
    event_rx: mpsc::UnboundedReceiver<SwapEvent>,
    /// Send commands to the network layer.
    command_tx: mpsc::UnboundedSender<ClientCommand>,
    /// Reference to accounting for balance updates.
    accounting: Arc<A>,
    /// Our exchange rate.
    our_rate: U256,
    /// Track pending outbound settlements (waiting for ack).
    pending: HashMap<OverlayAddress, PendingSettlement>,
}

struct PendingSettlement {
    amount: u64,
    response_tx: oneshot::Sender<Result<u64, SwapError>>,
}

impl<A: SwarmBandwidthAccounting + 'static> SwapService<A> {
    /// Create a new swap service.
    pub fn new(
        command_rx: mpsc::UnboundedReceiver<SwapCommand>,
        event_rx: mpsc::UnboundedReceiver<SwapEvent>,
        command_tx: mpsc::UnboundedSender<ClientCommand>,
        accounting: Arc<A>,
        our_rate: U256,
    ) -> Self {
        Self {
            command_rx,
            event_rx,
            command_tx,
            accounting,
            our_rate,
            pending: HashMap::new(),
        }
    }

    /// Run the service event loop.
    pub async fn run(mut self) {
        loop {
            tokio::select! {
                Some(cmd) = self.command_rx.recv() => {
                    self.handle_command(cmd).await;
                }
                Some(event) = self.event_rx.recv() => {
                    self.handle_event(event).await;
                }
                else => {
                    debug!("Swap service shutting down");
                    break;
                }
            }
        }
    }

    /// Convert self into a spawnable future.
    pub async fn into_task(self) {
        self.run().await;
    }

    async fn handle_command(&mut self, cmd: SwapCommand) {
        match cmd {
            SwapCommand::Settle {
                peer,
                amount,
                response_tx,
            } => {
                // Check if we already have a pending settlement with this peer
                if self.pending.contains_key(&peer) {
                    let _ = response_tx.send(Err(SwapError::SettlementInProgress));
                    return;
                }

                debug!(%peer, %amount, "Creating swap settlement cheque");

                // TODO: Sign cheque here
                // For now, create a placeholder cheque
                // This will require:
                // 1. ChequeSigner interface
                // 2. Last cumulative payout tracking
                // 3. Chequebook address

                // Store the pending settlement
                self.pending.insert(
                    peer,
                    PendingSettlement {
                        amount,
                        response_tx,
                    },
                );

                // For now, immediately complete with stub
                // In real implementation, we would send the cheque and wait for ChequeSent event
                warn!(%peer, %amount, "SWAP: Cheque signing not yet implemented - completing as stub");

                if let Some(pending) = self.pending.remove(&peer) {
                    // Credit our balance (we paid, debt reduced)
                    let handle = self.accounting.for_peer(peer);
                    handle.record(amount, Direction::Upload);

                    let _ = pending.response_tx.send(Ok(amount));
                }
            }
        }
    }

    async fn handle_event(&mut self, event: SwapEvent) {
        match event {
            SwapEvent::ChequeSent { peer, peer_rate } => {
                debug!(%peer, %peer_rate, "Cheque sent acknowledgment received");

                // Complete pending request
                if let Some(pending) = self.pending.remove(&peer) {
                    // Credit our balance
                    let handle = self.accounting.for_peer(peer);
                    handle.record(pending.amount, Direction::Upload);

                    let _ = pending.response_tx.send(Ok(pending.amount));
                } else {
                    warn!(%peer, "Received cheque sent ack for unknown settlement");
                }
            }
            SwapEvent::ChequeReceived {
                peer,
                cheque,
                peer_rate,
            } => {
                debug!(
                    %peer,
                    %peer_rate,
                    beneficiary = %cheque.cheque.beneficiary,
                    cumulative_payout = %cheque.cheque.cumulativePayout,
                    "Cheque received from peer"
                );

                // TODO: Validate cheque
                // 1. Verify EIP-712 signature
                // 2. Verify chequebook exists (on-chain)
                // 3. Verify issuer matches chequebook owner
                // 4. Verify sufficient balance (on-chain)
                // 5. Calculate actual amount (cumulative - last seen)

                // For now, just credit the full cumulative payout as a stub
                let amount = cheque.cheque.cumulativePayout.as_limbs()[0];

                // Credit peer's balance
                let handle = self.accounting.for_peer(peer);
                handle.record(amount, Direction::Download);

                warn!(%peer, %amount, "SWAP: Cheque validation not yet implemented - credited as stub");

                // TODO: Store cheque for later cashing
            }
        }
    }
}
