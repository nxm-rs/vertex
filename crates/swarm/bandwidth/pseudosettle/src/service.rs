//! Pseudosettle service actor (runs in its own tokio task).

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use alloy_primitives::U256;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};
use vertex_swarm_api::{Direction, SwarmBandwidthAccounting, SwarmPeerBandwidth};
use vertex_swarm_net_pseudosettle::PaymentAck;
use vertex_swarm_node::{ClientCommand, PseudosettleEvent};
use vertex_swarm_primitives::OverlayAddress;
use vertex_tasks::{GracefulShutdown, SpawnableTask};

use crate::error::PseudosettleSettlementError;

/// Commands from the handle to the service.
pub enum PseudosettleCommand {
    /// Request settlement with a peer.
    Settle {
        /// The peer to settle with.
        peer: OverlayAddress,
        /// The amount to settle.
        amount: u64,
        /// Channel to send the result.
        response_tx: oneshot::Sender<Result<u64, PseudosettleSettlementError>>,
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
    /// Tokens per second for rate limiting settlements.
    refresh_rate: u64,
    /// Track pending outbound settlements (waiting for ack).
    pending: HashMap<OverlayAddress, oneshot::Sender<Result<u64, PseudosettleSettlementError>>>,
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

    /// Run the service event loop with graceful shutdown support.
    async fn run(mut self, shutdown: GracefulShutdown) {
        let mut shutdown = std::pin::pin!(shutdown);

        loop {
            tokio::select! {
                guard = &mut shutdown => {
                    debug!("Pseudosettle service received shutdown signal");
                    drop(guard);
                    break;
                }
                Some(cmd) = self.command_rx.recv() => {
                    self.handle_command(cmd).await;
                }
                Some(event) = self.event_rx.recv() => {
                    self.handle_event(event).await;
                }
                else => {
                    debug!("Pseudosettle service channels closed");
                    break;
                }
            }
        }
        debug!("Pseudosettle service shutdown complete");
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
                    let _ =
                        response_tx.send(Err(PseudosettleSettlementError::SettlementInProgress));
                    return;
                }

                // Check rate limiting
                let now = current_timestamp();
                if let Some(&last) = self.last_settlement.get(&peer)
                    && now <= last
                {
                    let _ = response_tx.send(Err(PseudosettleSettlementError::TooSoon));
                    return;
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
                        let _ = tx.send(Err(PseudosettleSettlementError::NetworkError(
                            e.to_string(),
                        )));
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
                if let Some(&last) = self.last_settlement.get(&peer)
                    && now <= last
                {
                    // Too soon - ack with 0 amount
                    let ack = PaymentAck::now(U256::ZERO);
                    let _ = self.command_tx.send(ClientCommand::AckPseudosettle {
                        peer,
                        request_id,
                        ack,
                    });
                    return;
                }

                // Calculate acceptable amount based on time-based refresh
                let handle = self.accounting.for_peer(peer);
                let acceptable = self.calculate_acceptable(&peer, &handle, amount.as_limbs()[0]);

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

    /// Calculate acceptable amount, capped at what the peer owes us and the
    /// time-based allowance since the last settlement.
    fn calculate_acceptable(&self, peer: &OverlayAddress, handle: &A::Peer, requested: u64) -> u64 {
        let balance = handle.balance();

        // They can only pay us if they owe us (positive balance means they owe us)
        if balance <= 0 {
            return 0;
        }

        // Cap at what they actually owe us
        let owed = balance as u64;

        // Cap at time-based allowance: refresh_rate tokens accumulate per second
        let now = current_timestamp();
        let elapsed = self
            .last_settlement
            .get(peer)
            .map_or(now, |&last| now.saturating_sub(last));
        let allowance = self.refresh_rate.saturating_mul(elapsed);

        requested.min(owed).min(allowance)
    }
}

impl<A: SwarmBandwidthAccounting + 'static> SpawnableTask for PseudosettleService<A> {
    fn into_task(self, shutdown: GracefulShutdown) -> impl Future<Output = ()> + Send {
        self.run(shutdown)
    }
}

/// Get current timestamp in seconds.
fn current_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
