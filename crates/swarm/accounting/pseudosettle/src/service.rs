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
use vertex_swarm_client_protocol::{ClientCommand, PseudosettleAck, PseudosettleEvent};
use vertex_swarm_primitives::OverlayAddress;
use vertex_tasks::{GracefulShutdown, MaybeSend, SpawnableTask};

use crate::error::PseudosettleSettlementError;

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
    command_tx: mpsc::UnboundedSender<ClientCommand>,
    /// Reference to accounting for balance updates.
    accounting: Arc<A>,
    /// AU per second for rate limiting settlements.
    refresh_rate: Au,
    /// Track pending outbound settlements (waiting for ack).
    pending: HashMap<OverlayAddress, PendingSettlement>,
    /// Track last settlement time per peer (for rate limiting).
    last_settlement: HashMap<OverlayAddress, u64>,
    /// First time we started accounting for a peer's inbound settlements.
    ///
    /// The time-based allowance accrues from this point, never from the Unix
    /// epoch. On first contact, and after a reconnect that cleared
    /// `last_settlement`, the grant is bounded by the genuine wall-clock elapsed
    /// since this instant, not by the absolute timestamp.
    first_seen: HashMap<OverlayAddress, u64>,
    /// Optional reporter feeding settlement violations into peer scoring.
    reporter: Option<Arc<dyn PeerReporter>>,
}

impl<A: SwarmBandwidthAccounting + 'static> PseudosettleService<A> {
    /// Create a new pseudosettle service.
    pub fn new(
        command_rx: mpsc::UnboundedReceiver<PseudosettleCommand>,
        event_rx: mpsc::UnboundedReceiver<PseudosettleEvent>,
        command_tx: mpsc::UnboundedSender<ClientCommand>,
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
            first_seen: HashMap::new(),
            reporter: None,
        }
    }

    /// Attach a peer reporter so settlement violations feed peer scoring.
    ///
    /// Reporting is best-effort and non-blocking. Without a reporter the
    /// service behaves exactly as before.
    pub fn with_reporter(mut self, reporter: Arc<dyn PeerReporter>) -> Self {
        self.reporter = Some(reporter);
        self
    }

    /// Report an accounting violation if a reporter is attached.
    fn report_violation(&self, peer: &OverlayAddress) {
        if let Some(reporter) = &self.reporter {
            reporter.report_peer(
                peer,
                SwarmScoringEvent::AccountingViolation,
                ReportSource::Accounting,
            );
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

                // Store the offer and response channel for when we get the ack
                self.pending.insert(
                    peer,
                    PendingSettlement {
                        amount,
                        response_tx,
                    },
                );
                self.last_settlement.insert(peer, now);

                debug!(%peer, %amount, "Sending pseudosettle request");

                // Send via network
                if let Err(e) = self.command_tx.send(ClientCommand::SendPseudosettle {
                    peer,
                    amount: wire_from_au(amount),
                }) {
                    warn!(%peer, error = ?e, "Failed to send pseudosettle command");
                    // Remove the pending entry and notify failure
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
                debug!(%peer, amount = %ack.accepted, "Pseudosettle ack received");

                // Complete pending request with accepted amount
                if let Some(pending) = self.pending.remove(&peer) {
                    if ack.accepted > pending.amount {
                        // Law broken: an ack may accept at most the amount
                        // offered for settlement; over-acceptance desyncs
                        // the mutual books.
                        self.report_violation(&peer);
                    }

                    // Credit at most what we offered: an ack can never accept
                    // more than the offer, so a violating over-ack is clamped to
                    // the offer rather than inflating our credited balance.
                    let accepted = ack.accepted.min(pending.amount);

                    // Credit our balance (we paid, debt reduced)
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

                // Check rate limiting
                let now = current_timestamp();
                if let Some(&last) = self.last_settlement.get(&peer)
                    && now <= last
                {
                    // Too soon - ack with 0 amount
                    let ack = PseudosettleAck {
                        accepted: Au::ZERO,
                        timestamp: ack_timestamp(),
                    };
                    let _ = self.command_tx.send(ClientCommand::AckPseudosettle {
                        peer,
                        request_id,
                        ack,
                    });
                    return;
                }

                // Anchor the allowance clock the first time we account for this
                // peer (or after a reconnect cleared its state), so the elapsed
                // interval is genuine wall-clock, never the absolute timestamp.
                self.first_seen.entry(peer).or_insert(now);

                // Calculate acceptable amount based on time-based refresh
                let handle = self.accounting.for_peer(peer);
                let acceptable = self.calculate_acceptable(&peer, &handle, au_from_wire(amount));

                if acceptable.is_positive() {
                    // Credit peer's balance (they paid us)
                    handle.record(acceptable, Direction::Download);
                    self.last_settlement.insert(peer, now);
                }

                // Ack with accepted amount. The timestamp is sampled here, at the
                // decision point, so the peer refreshes its allowance against the
                // moment we decided; the wire boundary never re-samples it.
                let ack = PseudosettleAck {
                    accepted: acceptable,
                    timestamp: ack_timestamp(),
                };

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
    fn calculate_acceptable(&self, peer: &OverlayAddress, handle: &A::Peer, requested: Au) -> Au {
        let balance = handle.balance();

        // They can only pay us if they owe us (positive balance means they owe us)
        if !balance.is_positive() {
            return Au::ZERO;
        }

        // Cap at what they actually owe us
        let owed = balance;

        // Cap at time-based allowance: refresh_rate AU accumulate per second.
        // The elapsed interval is measured from the last settlement, or, with
        // none yet, from when we first started accounting for this peer. It is
        // never seeded from `now`: that would treat the whole Unix epoch as
        // elapsed and overflow the scaling into an unbounded grant, defeating
        // the only anti-free-ride brake on first contact and after a reconnect.
        // On overflow the allowance saturates, but the request and owed caps
        // below still bound the result.
        let now = current_timestamp();
        let since = self
            .last_settlement
            .get(peer)
            .or_else(|| self.first_seen.get(peer))
            .copied()
            .unwrap_or(now);
        let elapsed = now.saturating_sub(since);
        let allowance = self
            .refresh_rate
            .checked_scale(elapsed)
            .unwrap_or(Au::from_amount(u64::MAX));

        requested.min(owed).min(allowance)
    }
}

/// Convert a wire settlement amount (`U256`) into AU.
///
/// Pseudosettle amounts on the wire are at most a `u64` of AU, so this takes
/// the low 64 bits, matching the legacy behaviour. It is the only `U256` to AU
/// crossing in this crate.
fn au_from_wire(amount: U256) -> Au {
    Au::from_amount(amount.as_limbs()[0])
}

/// Convert an AU amount into the wire settlement representation (`U256`).
///
/// The only AU to `U256` crossing in this crate.
fn wire_from_au(amount: Au) -> U256 {
    U256::from(amount.as_amount())
}

impl<A: SwarmBandwidthAccounting + 'static> SpawnableTask for PseudosettleService<A> {
    fn into_task(self, shutdown: GracefulShutdown) -> impl Future<Output = ()> + MaybeSend {
        self.run(shutdown)
    }
}

/// Get current timestamp in seconds.
fn current_timestamp() -> u64 {
    vertex_util_runtime::time::now_unix_secs()
}

/// Sample the clock for an outbound ack timestamp, in Unix nanoseconds.
///
/// The peer refreshes its allowance against this value, so it is sampled at the
/// decision point and carried through to the wire boundary unchanged.
fn ack_timestamp() -> i64 {
    vertex_util_runtime::time::now_unix_nanos()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use vertex_swarm_accounting::{Accounting, BandwidthConfig};
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

    // A received ack accepting `amount` AU, as the wire boundary would deliver it.
    fn sent_ack(amount: u64) -> PseudosettleAck {
        PseudosettleAck {
            accepted: Au::from_amount(amount),
            timestamp: ack_timestamp(),
        }
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
            ack: sent_ack(200),
        })
        .await;

        assert_eq!(reporter.reports.lock().len(), 1);
        let (reported_peer, event, source) = reporter.reports.lock()[0];
        assert_eq!(reported_peer, peer);
        assert_eq!(event, SwarmScoringEvent::AccountingViolation);
        assert_eq!(source, ReportSource::Accounting);
        // The over-ack is clamped to the offer: we credit at most what we
        // offered, never the inflated acked amount.
        assert_eq!(rx.try_recv().unwrap().unwrap(), Au::from_amount(100));

        // Every over-acceptance ack is an independent wire-level violation,
        // so a repeat offence reports again (no debounce).
        let _rx = insert_pending(&mut svc, peer, Au::from_amount(100));
        svc.handle_event(PseudosettleEvent::Sent {
            peer,
            ack: sent_ack(300),
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
            ack: sent_ack(100),
        })
        .await;
        assert_eq!(rx.try_recv().unwrap().unwrap(), Au::from_amount(100));

        // Underpayment (time-capped acceptance) is lawful too.
        let mut rx = insert_pending(&mut svc, peer, Au::from_amount(100));
        svc.handle_event(PseudosettleEvent::Sent {
            peer,
            ack: sent_ack(10),
        })
        .await;
        assert_eq!(rx.try_recv().unwrap().unwrap(), Au::from_amount(10));

        // An ack with no pending settlement is ignored, not reported: it can
        // be a local race rather than provable peer misbehaviour.
        svc.handle_event(PseudosettleEvent::Sent {
            peer,
            ack: sent_ack(10),
        })
        .await;

        assert!(reporter.reports.lock().is_empty());
    }

    // The peer owes us far more than any plausible time-based allowance, so
    // the only thing capping a grant is `refresh_rate * elapsed`. Returns the
    // service (with the owed balance recorded) and the per-second refresh rate.
    fn service_with_large_debt(peer: OverlayAddress, refresh_rate: Au) -> TestService {
        let (_cmd_tx, command_rx) = mpsc::unbounded_channel();
        let (_evt_tx, event_rx) = mpsc::unbounded_channel();
        let (client_tx, _client_rx) = mpsc::unbounded_channel();
        let accounting = Arc::new(Accounting::new(BandwidthConfig::default(), test_identity()));
        let svc =
            PseudosettleService::new(command_rx, event_rx, client_tx, accounting, refresh_rate);
        // Peer owes us a very large amount (positive balance).
        svc.accounting
            .for_peer(peer)
            .record(Au::from_amount(1_000_000_000_000), Direction::Upload);
        svc
    }

    #[test]
    fn first_contact_grant_is_bounded_by_elapsed_not_unbounded() {
        let peer = test_peer();
        let refresh_rate = Au::from_amount(4_500_000);
        let mut svc = service_with_large_debt(peer, refresh_rate);

        // First contact: anchor the allowance clock at `now`, exactly as the
        // inbound `Received` path does before computing the acceptable amount.
        let now = current_timestamp();
        svc.first_seen.insert(peer, now);
        assert!(!svc.last_settlement.contains_key(&peer));

        let handle = svc.accounting.for_peer(peer);
        let requested = Au::from_amount(1_000_000_000_000);
        let acceptable = svc.calculate_acceptable(&peer, &handle, requested);

        // With elapsed near zero the grant must be a tiny multiple of the
        // refresh rate, never the full debt. Allow a small wall-clock slack.
        let ceiling = refresh_rate.checked_scale(5).unwrap();
        assert!(
            acceptable <= ceiling,
            "first-contact grant {acceptable} exceeded the elapsed-bounded ceiling {ceiling}"
        );
        // Pre-fix this saturated to the full debt; prove it is genuinely small.
        assert!(acceptable < Au::from_amount(1_000_000_000));
    }

    #[test]
    fn reconnect_does_not_reset_to_unbounded_grant() {
        let peer = test_peer();
        let refresh_rate = Au::from_amount(4_500_000);
        let mut svc = service_with_large_debt(peer, refresh_rate);

        // Simulate a reconnect: `last_settlement` is cleared (in-memory state
        // lost), but the inbound path re-anchors `first_seen` to now.
        let now = current_timestamp();
        svc.last_settlement.remove(&peer);
        svc.first_seen.insert(peer, now);

        let handle = svc.accounting.for_peer(peer);
        let acceptable =
            svc.calculate_acceptable(&peer, &handle, Au::from_amount(1_000_000_000_000));

        let ceiling = refresh_rate.checked_scale(5).unwrap();
        assert!(
            acceptable <= ceiling,
            "post-reconnect grant {acceptable} exceeded the elapsed-bounded ceiling {ceiling}"
        );
    }

    #[tokio::test]
    async fn over_ack_is_clamped_to_offer() {
        let mut svc = build_service();
        let peer = test_peer();

        // We offered 100 but the peer acks 250: credit is clamped to 100.
        let mut rx = insert_pending(&mut svc, peer, Au::from_amount(100));
        svc.handle_event(PseudosettleEvent::Sent {
            peer,
            ack: sent_ack(250),
        })
        .await;

        assert_eq!(rx.try_recv().unwrap().unwrap(), Au::from_amount(100));
        let handle = svc.accounting.for_peer(peer);
        assert_eq!(handle.balance(), Au::from_amount(100));
    }

    #[tokio::test]
    async fn no_reporter_behaviour_unchanged() {
        let mut svc = build_service();
        let peer = test_peer();

        let mut rx = insert_pending(&mut svc, peer, Au::from_amount(100));
        svc.handle_event(PseudosettleEvent::Sent {
            peer,
            ack: sent_ack(200),
        })
        .await;

        // Same outcome as with a reporter: the ack completes the settlement,
        // and the over-ack is clamped to the offer with or without reporting.
        assert_eq!(rx.try_recv().unwrap().unwrap(), Au::from_amount(100));
        let handle = svc.accounting.for_peer(peer);
        assert_eq!(handle.balance(), Au::from_amount(100));
    }
}
