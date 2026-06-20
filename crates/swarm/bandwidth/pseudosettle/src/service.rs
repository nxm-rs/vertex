//! Pseudosettle service actor (runs in its own tokio task).

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use alloy_primitives::U256;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};
use vertex_swarm_api::{
    Au, Direction, PeerReporter, ReportSource, SwarmBandwidthAccounting, SwarmPeerBandwidth,
    SwarmScoringEvent,
};
use vertex_swarm_net_pseudosettle::PaymentAck;
use vertex_swarm_primitives::OverlayAddress;
use vertex_tasks::{GracefulShutdown, SpawnableTask};

use crate::error::PseudosettleSettlementError;
use crate::event::{PseudosettleEvent, PseudosettleNetworkCommand};

/// Commands from the handle to the service.
pub enum PseudosettleCommand {
    /// Request settlement with a peer.
    Settle {
        /// The peer to settle with.
        peer: OverlayAddress,
        /// The amount to settle in AU.
        amount: Au,
        /// Channel to send the result.
        response_tx: oneshot::Sender<Result<Au, PseudosettleSettlementError>>,
    },
}

/// An outbound settlement awaiting the peer's ack.
struct PendingSettlement {
    /// The amount in AU we offered to settle.
    amount: Au,
    /// Channel completing the originating settle request.
    response_tx: oneshot::Sender<Result<Au, PseudosettleSettlementError>>,
}

/// Processes settlement commands from handles and network events.
pub struct PseudosettleService<A: SwarmBandwidthAccounting> {
    /// Receive commands from handles.
    command_rx: mpsc::UnboundedReceiver<PseudosettleCommand>,
    /// Receive events routed from the network layer.
    event_rx: mpsc::UnboundedReceiver<PseudosettleEvent>,
    /// Send commands to the network layer.
    command_tx: mpsc::UnboundedSender<PseudosettleNetworkCommand>,
    /// Reference to accounting for balance updates.
    accounting: Arc<A>,
    /// AU per second for rate limiting settlements.
    refresh_rate: Au,
    /// Track pending outbound settlements (waiting for ack).
    pending: HashMap<OverlayAddress, PendingSettlement>,
    /// Track last settlement time per peer (for rate limiting).
    last_settlement: HashMap<OverlayAddress, u64>,
    /// Optional reporter feeding settlement violations into peer scoring.
    reporter: Option<Arc<dyn PeerReporter>>,
}

impl<A: SwarmBandwidthAccounting + 'static> PseudosettleService<A> {
    /// Create a new pseudosettle service.
    pub fn new(
        command_rx: mpsc::UnboundedReceiver<PseudosettleCommand>,
        event_rx: mpsc::UnboundedReceiver<PseudosettleEvent>,
        command_tx: mpsc::UnboundedSender<PseudosettleNetworkCommand>,
        accounting: Arc<A>,
        refresh_rate: Au,
    ) -> Self {
        Self {
            command_rx,
            event_rx,
            command_tx,
            accounting,
            refresh_rate,
            pending: HashMap::new(),
            last_settlement: HashMap::new(),
            reporter: None,
        }
    }

    /// Attach a peer reporter so settlement violations feed peer scoring.
    pub fn with_reporter(mut self, reporter: Arc<dyn PeerReporter>) -> Self {
        self.reporter = Some(reporter);
        self
    }

    fn report_violation(&self, peer: &OverlayAddress) {
        if let Some(reporter) = &self.reporter {
            reporter.report_peer(
                peer,
                SwarmScoringEvent::AccountingViolation,
                ReportSource::Accounting,
            );
        }
    }

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
                if self.pending.contains_key(&peer) {
                    let _ =
                        response_tx.send(Err(PseudosettleSettlementError::SettlementInProgress));
                    return;
                }

                let now = current_timestamp();
                if let Some(&last) = self.last_settlement.get(&peer)
                    && now <= last
                {
                    let _ = response_tx.send(Err(PseudosettleSettlementError::TooSoon));
                    return;
                }

                self.pending.insert(
                    peer,
                    PendingSettlement {
                        amount,
                        response_tx,
                    },
                );
                self.last_settlement.insert(peer, now);

                debug!(%peer, %amount, "Sending pseudosettle request");

                if let Err(e) = self.command_tx.send(PseudosettleNetworkCommand::Send {
                    peer,
                    amount: wire_from_au(amount),
                }) {
                    warn!(%peer, error = ?e, "Failed to send pseudosettle command");
                    if let Some(pending) = self.pending.remove(&peer) {
                        let _ = pending.response_tx.send(Err(
                            PseudosettleSettlementError::NetworkError(e.to_string()),
                        ));
                    }
                }
            }
        }
    }

    async fn handle_event(&mut self, event: PseudosettleEvent) {
        match event {
            PseudosettleEvent::Sent { peer, ack } => {
                debug!(%peer, amount = %ack.amount, "Pseudosettle ack received");

                if let Some(pending) = self.pending.remove(&peer) {
                    // An ack may accept at most the offered amount; over-acceptance
                    // desyncs the mutual books.
                    if ack.amount > wire_from_au(pending.amount) {
                        self.report_violation(&peer);
                    }

                    let accepted = au_from_wire(ack.amount);

                    let handle = self.accounting.for_peer(peer);
                    handle.record(accepted, Direction::Upload);

                    let _ = pending.response_tx.send(Ok(accepted));
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

                let now = current_timestamp();
                if let Some(&last) = self.last_settlement.get(&peer)
                    && now <= last
                {
                    let ack = PaymentAck::now(U256::ZERO);
                    let _ = self.command_tx.send(PseudosettleNetworkCommand::Ack {
                        peer,
                        request_id,
                        ack,
                    });
                    return;
                }

                let handle = self.accounting.for_peer(peer);
                let acceptable = self.calculate_acceptable(&peer, &handle, au_from_wire(amount));

                if acceptable.is_positive() {
                    handle.record(acceptable, Direction::Download);
                    self.last_settlement.insert(peer, now);
                }

                let ack = PaymentAck::now(wire_from_au(acceptable));

                debug!(%peer, %acceptable, "Sending pseudosettle ack");

                if let Err(e) = self.command_tx.send(PseudosettleNetworkCommand::Ack {
                    peer,
                    request_id,
                    ack,
                }) {
                    warn!(%peer, error = ?e, "Failed to send pseudosettle ack");
                }
            }
        }
    }

    /// Acceptable amount, capped at what the peer owes us and the time-based
    /// allowance since the last settlement.
    fn calculate_acceptable(&self, peer: &OverlayAddress, handle: &A::Peer, requested: Au) -> Au {
        let balance = handle.balance();

        // A positive balance means they owe us; only then can they pay.
        if !balance.is_positive() {
            return Au::ZERO;
        }

        let owed = balance;

        // Checked scaling so a large rate over a long gap cannot wrap into a tiny
        // allowance; on overflow it saturates and the caps below still bound it.
        let now = current_timestamp();
        let elapsed = self
            .last_settlement
            .get(peer)
            .map_or(now, |&last| now.saturating_sub(last));
        let allowance = self
            .refresh_rate
            .checked_scale(elapsed)
            .unwrap_or(Au::from_amount(u64::MAX));

        requested.min(owed).min(allowance)
    }
}

/// Wire settlement amount (`U256`) to AU. Wire amounts are at most a `u64` of
/// AU, so this takes the low 64 bits. The only `U256` to AU crossing here.
fn au_from_wire(amount: U256) -> Au {
    Au::from_amount(amount.as_limbs()[0])
}

/// AU to its wire representation (`U256`). The only AU to `U256` crossing here.
fn wire_from_au(amount: Au) -> U256 {
    U256::from(amount.as_amount())
}

impl<A: SwarmBandwidthAccounting + 'static> SpawnableTask for PseudosettleService<A> {
    fn into_task(self, shutdown: GracefulShutdown) -> impl Future<Output = ()> + Send {
        self.run(shutdown)
    }
}

fn current_timestamp() -> u64 {
    vertex_util_runtime::time::now_unix_secs()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use vertex_swarm_bandwidth::{Accounting, BandwidthConfig};
    use vertex_swarm_test_utils::{Identity, test_identity, test_peer};

    type TestService = PseudosettleService<Accounting<BandwidthConfig, Identity>>;

    #[derive(Default)]
    struct RecordingReporter {
        reports: parking_lot::Mutex<Vec<(OverlayAddress, SwarmScoringEvent, ReportSource)>>,
    }

    impl PeerReporter for RecordingReporter {
        fn report_peer(
            &self,
            overlay: &OverlayAddress,
            event: SwarmScoringEvent,
            source: ReportSource,
        ) {
            self.reports.lock().push((*overlay, event, source));
        }
    }

    fn build_service() -> TestService {
        let (_cmd_tx, command_rx) = mpsc::unbounded_channel();
        let (_evt_tx, event_rx) = mpsc::unbounded_channel();
        let (client_tx, _client_rx) = mpsc::unbounded_channel();
        let accounting = Arc::new(Accounting::new(BandwidthConfig::default(), test_identity()));

        PseudosettleService::new(
            command_rx,
            event_rx,
            client_tx,
            accounting,
            Au::from_amount(4_500_000),
        )
    }

    fn insert_pending(
        svc: &mut TestService,
        peer: OverlayAddress,
        amount: Au,
    ) -> oneshot::Receiver<Result<Au, PseudosettleSettlementError>> {
        let (response_tx, response_rx) = oneshot::channel();
        svc.pending.insert(
            peer,
            PendingSettlement {
                amount,
                response_tx,
            },
        );
        response_rx
    }

    #[tokio::test]
    async fn over_acceptance_reports_violation_per_message() {
        let reporter = Arc::new(RecordingReporter::default());
        let mut svc = build_service().with_reporter(Arc::clone(&reporter) as Arc<dyn PeerReporter>);
        let peer = test_peer();

        // The peer acks more than we offered.
        let mut rx = insert_pending(&mut svc, peer, Au::from_amount(100));
        svc.handle_event(PseudosettleEvent::Sent {
            peer,
            ack: PaymentAck::now(U256::from(200u64)),
        })
        .await;

        assert_eq!(reporter.reports.lock().len(), 1);
        let (reported_peer, event, source) = reporter.reports.lock()[0];
        assert_eq!(reported_peer, peer);
        assert_eq!(event, SwarmScoringEvent::AccountingViolation);
        assert_eq!(source, ReportSource::Accounting);
        // The accounting effect of the ack is unchanged by reporting.
        assert_eq!(rx.try_recv().unwrap().unwrap(), Au::from_amount(200));

        // Each over-acceptance is an independent violation, so it reports again.
        let _rx = insert_pending(&mut svc, peer, Au::from_amount(100));
        svc.handle_event(PseudosettleEvent::Sent {
            peer,
            ack: PaymentAck::now(U256::from(300u64)),
        })
        .await;
        assert_eq!(reporter.reports.lock().len(), 2);
    }

    #[tokio::test]
    async fn lawful_acks_do_not_report() {
        let reporter = Arc::new(RecordingReporter::default());
        let mut svc = build_service().with_reporter(Arc::clone(&reporter) as Arc<dyn PeerReporter>);
        let peer = test_peer();

        // Full acceptance is lawful.
        let mut rx = insert_pending(&mut svc, peer, Au::from_amount(100));
        svc.handle_event(PseudosettleEvent::Sent {
            peer,
            ack: PaymentAck::now(U256::from(100u64)),
        })
        .await;
        assert_eq!(rx.try_recv().unwrap().unwrap(), Au::from_amount(100));

        // Underpayment (time-capped acceptance) is lawful too.
        let mut rx = insert_pending(&mut svc, peer, Au::from_amount(100));
        svc.handle_event(PseudosettleEvent::Sent {
            peer,
            ack: PaymentAck::now(U256::from(10u64)),
        })
        .await;
        assert_eq!(rx.try_recv().unwrap().unwrap(), Au::from_amount(10));

        // An ack with no pending settlement is ignored, not reported: it can be
        // a local race rather than provable misbehaviour.
        svc.handle_event(PseudosettleEvent::Sent {
            peer,
            ack: PaymentAck::now(U256::from(10u64)),
        })
        .await;

        assert!(reporter.reports.lock().is_empty());
    }

    #[tokio::test]
    async fn no_reporter_behaviour_unchanged() {
        let mut svc = build_service();
        let peer = test_peer();

        let mut rx = insert_pending(&mut svc, peer, Au::from_amount(100));
        svc.handle_event(PseudosettleEvent::Sent {
            peer,
            ack: PaymentAck::now(U256::from(200u64)),
        })
        .await;

        // Same outcome as with a reporter: the ack completes the settlement.
        assert_eq!(rx.try_recv().unwrap().unwrap(), Au::from_amount(200));
        let handle = svc.accounting.for_peer(peer);
        assert_eq!(handle.balance(), Au::from_amount(200));
    }
}
