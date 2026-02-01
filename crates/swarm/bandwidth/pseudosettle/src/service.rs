//! Pseudosettle service actor (runs in its own tokio task).

use std::collections::HashMap;
use std::sync::Arc;

use alloy_primitives::U256;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};
use vertex_net_pseudosettle::PaymentAck;
use vertex_swarm_api::{Direction, SwarmBandwidthAccounting, SwarmPeerBandwidth};
use vertex_swarm_client::{PseudosettleEvent, protocol::ClientCommand};
use vertex_swarm_primitives::OverlayAddress;

use crate::error::PseudosettleError;

/// Commands from the handle to the service.
pub enum PseudosettleCommand {
    /// Request settlement with a peer.
    Settle {
        /// The peer to settle with.
        peer: OverlayAddress,
        /// The amount to settle.
        amount: u64,
        /// Channel to send the result.
        response_tx: oneshot::Sender<Result<u64, PseudosettleError>>,
    },
}

/// Processes settlement commands from handles and network events.
pub struct PseudosettleService<A: SwarmBandwidthAccounting> {
    /// Receive commands from handles.
    command_rx: mpsc::UnboundedReceiver<PseudosettleCommand>,
    /// Receive events routed from the network layer.
    event_rx: mpsc::UnboundedReceiver<PseudosettleEvent>,
    /// Send commands to the network layer.
    command_tx: mpsc::UnboundedSender<ClientCommand>,
    /// Reference to accounting for balance updates.
    accounting: Arc<A>,
    /// Config for refresh rate (tokens per second).
    refresh_rate: u64,
    /// Track pending outbound settlements (waiting for ack).
    pending: HashMap<OverlayAddress, oneshot::Sender<Result<u64, PseudosettleError>>>,
    /// Track last settlement time per peer (for rate limiting).
    last_settlement: HashMap<OverlayAddress, u64>,
}

impl<A: SwarmBandwidthAccounting + 'static> PseudosettleService<A> {
    /// Create a new pseudosettle service.
    pub fn new(
        command_rx: mpsc::UnboundedReceiver<PseudosettleCommand>,
        event_rx: mpsc::UnboundedReceiver<PseudosettleEvent>,
        command_tx: mpsc::UnboundedSender<ClientCommand>,
        accounting: Arc<A>,
        refresh_rate: u64,
    ) -> Self {
        Self {
            command_rx,
            event_rx,
            command_tx,
            accounting,
            refresh_rate,
            pending: HashMap::new(),
            last_settlement: HashMap::new(),
        }
    }

    /// Run the service event loop.
    ///
    /// This method runs until all senders are dropped.
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
                    debug!("Pseudosettle service shutting down");
                    break;
                }
            }
        }
    }

    /// Convert self into a spawnable future.
    pub async fn into_task(self) {
        self.run().await;
    }

    async fn handle_command(&mut self, cmd: PseudosettleCommand) {
        match cmd {
            PseudosettleCommand::Settle {
                peer,
                amount,
                response_tx,
            } => {
                // Check if we already have a pending settlement with this peer
                if self.pending.contains_key(&peer) {
                    let _ = response_tx.send(Err(PseudosettleError::SettlementInProgress));
                    return;
                }

                // Check rate limiting (1 second minimum between settlements)
                let now = current_timestamp();
                if let Some(&last) = self.last_settlement.get(&peer) {
                    if now <= last {
                        let _ = response_tx.send(Err(PseudosettleError::TooSoon));
                        return;
                    }
                }

                // Store the response channel for when we get the ack
                self.pending.insert(peer, response_tx);
                self.last_settlement.insert(peer, now);

                debug!(%peer, %amount, "Sending pseudosettle request");

                // Send via network
                if let Err(e) = self.command_tx.send(ClientCommand::SendPseudosettle {
                    peer,
                    amount: U256::from(amount),
                }) {
                    warn!(%peer, error = ?e, "Failed to send pseudosettle command");
                    // Remove the pending entry and notify failure
                    if let Some(tx) = self.pending.remove(&peer) {
                        let _ = tx.send(Err(PseudosettleError::NetworkError(e.to_string())));
                    }
                }
            }
        }
    }

    async fn handle_event(&mut self, event: PseudosettleEvent) {
        match event {
            PseudosettleEvent::Sent { peer, ack } => {
                debug!(%peer, amount = %ack.amount, "Pseudosettle ack received");

                // Complete pending request with accepted amount
                if let Some(tx) = self.pending.remove(&peer) {
                    // Credit our balance (we paid, debt reduced)
                    let handle = self.accounting.for_peer(peer);
                    handle.record(ack.amount.as_limbs()[0], Direction::Upload);

                    let _ = tx.send(Ok(ack.amount.as_limbs()[0]));
                } else {
                    warn!(%peer, "Received ack for unknown settlement");
                }
            }
            PseudosettleEvent::Received {
                peer,
                amount,
                request_id,
            } => {
                debug!(%peer, %amount, %request_id, "Pseudosettle request received");

                // Check rate limiting
                let now = current_timestamp();
                if let Some(&last) = self.last_settlement.get(&peer) {
                    if now <= last {
                        // Too soon - ack with 0 amount
                        let ack = PaymentAck::now(U256::ZERO);
                        let _ = self.command_tx.send(ClientCommand::AckPseudosettle {
                            peer,
                            request_id,
                            ack,
                        });
                        return;
                    }
                }

                // Calculate acceptable amount based on time-based refresh
                let handle = self.accounting.for_peer(peer);
                let acceptable = self.calculate_acceptable(&handle, amount.as_limbs()[0]);

                if acceptable > 0 {
                    // Credit peer's balance (they paid us)
                    handle.record(acceptable, Direction::Download);
                    self.last_settlement.insert(peer, now);
                }

                // Ack with accepted amount
                let ack = PaymentAck::now(U256::from(acceptable));

                debug!(%peer, %acceptable, "Sending pseudosettle ack");

                if let Err(e) = self.command_tx.send(ClientCommand::AckPseudosettle {
                    peer,
                    request_id,
                    ack,
                }) {
                    warn!(%peer, error = ?e, "Failed to send pseudosettle ack");
                }
            }
        }
    }

    /// Calculate acceptable amount, capped at what the peer owes us.
    fn calculate_acceptable(&self, handle: &A::Peer, requested: u64) -> u64 {
        let balance = handle.balance();

        // They can only pay us if they owe us (positive balance means they owe us)
        if balance <= 0 {
            return 0;
        }

        // Cap at what they actually owe us
        let owed = balance as u64;
        let capped = std::cmp::min(requested, owed);

        // Also cap at time-based allowance
        // For now, we accept up to the capped amount
        // TODO: Implement proper time-based allowance tracking per peer
        // This would involve tracking accumulated allowance since last settlement
        capped
    }
}

/// Get current timestamp in seconds.
fn current_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
