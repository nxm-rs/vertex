//! Pseudosettle service actor (runs in its own tokio task).

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use alloy_primitives::U256;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};
use vertex_swarm_api::{
    Au, Direction, PeerReporter, ReportSource, SwarmAccounting, SwarmPeerAccounting,
    SwarmScoringEvent,
};
use vertex_swarm_client_protocol::{
    ClientCommand, PeerCommand, PseudosettleAck, PseudosettleEvent,
};
use vertex_swarm_primitives::OverlayAddress;
use vertex_tasks::{GracefulShutdown, MaybeSend, SpawnableTask};

use crate::error::PseudosettleSettlementError;

/// Clock-skew tolerance, in seconds, for trusting a creditor's ack timestamp as
/// our next-settle reference. An ack within this window below our clock (and not
/// in the future) is trusted; anything outside falls back to our clock. 60s
/// covers honest peer/NTP skew while denying two attacks: future-freeze, where a
/// timestamp ahead of our clock pushes the gate beyond reach and stalls our
/// settling until the creditor drops us; and rewind-spam, where an ever-older
/// reported timestamp rewinds the gate to induce settle spam (countered by
/// keeping the stored reference monotonic non-decreasing).
const CLOCK_SKEW_WINDOW_SECS: u64 = 60;

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
pub struct PseudosettleService<A: SwarmAccounting> {
    /// Receive commands from handles.
    command_rx: mpsc::UnboundedReceiver<PseudosettleCommand>,
    /// Receive events routed from the network layer.
    event_rx: mpsc::UnboundedReceiver<PseudosettleEvent>,
    /// Send commands to the network layer.
    command_tx: mpsc::UnboundedSender<ClientCommand>,
    /// Reference to accounting for balance updates.
    accounting: Arc<A>,
    /// Track pending outbound settlements (waiting for ack).
    pending: HashMap<OverlayAddress, PendingSettlement>,
    /// Our own clock at the last inbound credit per peer; the creditor-side
    /// reference read by the inbound rate gate and `calculate_acceptable`.
    last_settlement: HashMap<OverlayAddress, u64>,
    /// Creditor's clamped, monotonic ack timestamp per peer; paces our next
    /// OUTBOUND settle so we do not re-send before the creditor can forgive
    /// again. Distinct from `last_settlement`, which is our own clock for the
    /// INBOUND creditor allowance.
    last_settle_ack: HashMap<OverlayAddress, u64>,
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

impl<A: SwarmAccounting + 'static> PseudosettleService<A> {
    /// Create a new pseudosettle service.
    pub fn new(
        command_rx: mpsc::UnboundedReceiver<PseudosettleCommand>,
        event_rx: mpsc::UnboundedReceiver<PseudosettleEvent>,
        command_tx: mpsc::UnboundedSender<ClientCommand>,
        accounting: Arc<A>,
    ) -> Self {
        Self {
            command_rx,
            event_rx,
            command_tx,
            accounting,
            pending: HashMap::new(),
            last_settlement: HashMap::new(),
            last_settle_ack: HashMap::new(),
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
                if let Some(&last) = self.last_settle_ack.get(&peer)
                    && now <= last
                {
                    let _ = response_tx.send(Err(PseudosettleSettlementError::TooSoon));
                    return;
                }

                // The next-settle reference is set from the creditor's ack in
                // the `Sent` handler; `pending` already serialises concurrent
                // settles to one peer.
                self.pending.insert(
                    peer,
                    PendingSettlement {
                        amount,
                        response_tx,
                    },
                );

                debug!(%peer, %amount, "Sending pseudosettle request");

                // Send via network
                if let Err(e) = self.command_tx.send(ClientCommand::Peer {
                    peer,
                    command: PeerCommand::SendPseudosettle {
                        amount: wire_from_au(amount),
                    },
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

                    // Pace the next outbound settle off the creditor's clock:
                    // it refreshes our allowance against the timestamp it
                    // reports here, so gating on that stops us re-sending before
                    // it has anything to forgive. The timestamp is clamped to
                    // our clock and kept monotonic to deny two attacks (see
                    // CLOCK_SKEW_WINDOW_SECS).
                    let now = current_timestamp();
                    let window = now.saturating_sub(CLOCK_SKEW_WINDOW_SECS)..=now;
                    let effective = u64::try_from(ack.timestamp)
                        .ok()
                        .filter(|t| window.contains(t))
                        .unwrap_or(now);
                    self.last_settle_ack
                        .entry(peer)
                        .and_modify(|prev| *prev = (*prev).max(effective))
                        .or_insert(effective);

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
                    let _ = self.command_tx.send(ClientCommand::Peer {
                        peer,
                        command: PeerCommand::AckPseudosettle { request_id, ack },
                    });
                    return;
                }

                // Anchor the allowance clock the first time we account for this
                // peer (or after a reconnect cleared its state), so the elapsed
                // interval is genuine wall-clock, never the absolute timestamp.
                self.first_seen.entry(peer).or_insert(now);

                // Calculate acceptable amount based on time-based refresh
                let handle = self.accounting.for_peer(peer);
                let acceptable =
                    self.calculate_acceptable(&peer, &handle, Au::saturating_from_u256(amount));

                if acceptable.is_positive() {
                    // Credit the ledger and accumulate the peer's repayment
                    // total through the one settlement-received seam.
                    handle.settlement_received(acceptable);
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

                if let Err(e) = self.command_tx.send(ClientCommand::Peer {
                    peer,
                    command: PeerCommand::AckPseudosettle { request_id, ack },
                }) {
                    warn!(%peer, error = ?e, "Failed to send pseudosettle ack");
                }
            }
            PseudosettleEvent::Failed { peer } => {
                // The substream died or the peer disconnected before acking, so
                // resolve the pending settle with an error rather than leaking it.
                if let Some(pending) = self.pending.remove(&peer) {
                    debug!(%peer, "Pseudosettle substream failed; releasing pending settle");
                    let _ =
                        pending
                            .response_tx
                            .send(Err(PseudosettleSettlementError::NetworkError(
                                "substream failed".into(),
                            )));
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

        // Cap at the time-based allowance: the peer's allowance rate (keyed on
        // its handshake node type at connect) accumulates per second. The
        // elapsed interval is measured from the last settlement, or, with
        // none yet, from when we first started accounting for this peer. The
        // anchor must be a recorded wall-clock instant: deriving `elapsed`
        // from an absolute epoch seed (timestamp zero) rather than a recorded
        // anchor would scale the whole epoch into an unbounded grant,
        // defeating the only anti-free-ride brake on first contact and after
        // a reconnect.
        // On overflow the allowance saturates, but the request and owed caps
        // below still bound the result.
        // A missing anchor grants zero; the allowance is never derived from the clock alone.
        let now = current_timestamp();
        let Some(since) = self
            .last_settlement
            .get(peer)
            .or_else(|| self.first_seen.get(peer))
            .copied()
        else {
            debug!(%peer, "no allowance anchor recorded, granting zero");
            return Au::ZERO;
        };
        let elapsed = now.saturating_sub(since);
        let allowance = handle
            .refresh_allowance()
            .checked_scale(elapsed)
            .unwrap_or(Au::from_amount(u64::MAX));

        requested.min(owed).min(allowance)
    }
}

/// Convert an AU amount into the wire settlement representation (`U256`).
///
/// The only AU to `U256` crossing in this crate. The amount is always
/// non-negative (an offer or a `min` of non-negative caps), so the clamp in
/// [`Au::as_amount`] never engages.
fn wire_from_au(amount: Au) -> U256 {
    U256::from(amount.as_amount())
}

impl<A: SwarmAccounting + 'static> SpawnableTask for PseudosettleService<A> {
    fn into_task(self, shutdown: GracefulShutdown) -> impl Future<Output = ()> + MaybeSend {
        self.run(shutdown)
    }
}

/// Get current timestamp in seconds.
fn current_timestamp() -> u64 {
    vertex_util_runtime::time::now_unix_secs()
}

/// Sample the clock for an outbound ack timestamp, in Unix seconds.
///
/// The payer rejects an ack whose timestamp is more than a couple of seconds
/// off its own clock, so this must be seconds, not nanoseconds, to interoperate.
fn ack_timestamp() -> i64 {
    current_timestamp() as i64
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use vertex_swarm_accounting::{Accounting, AccountingConfig};
    use vertex_swarm_api::SwarmNodeType;
    use vertex_swarm_test_utils::{Identity, test_identity, test_peer};

    type TestService = PseudosettleService<Accounting<AccountingConfig, Identity>>;

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
        let accounting = Arc::new(Accounting::new(
            AccountingConfig::default(),
            test_identity(),
        ));

        PseudosettleService::new(command_rx, event_rx, client_tx, accounting)
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

    // A distinct deterministic peer per `n`, since `test_peer` always returns
    // the same address and these tests gate several peers independently.
    fn peer_n(n: u8) -> OverlayAddress {
        OverlayAddress::from([n; 32])
    }

    // A service whose outbound command channel stays open, so a settle that
    // passes the rate gate actually emits a `SendPseudosettle` instead of
    // failing with a dropped-receiver network error.
    fn build_service_with_rx() -> (TestService, mpsc::UnboundedReceiver<ClientCommand>) {
        let (_cmd_tx, command_rx) = mpsc::unbounded_channel();
        let (_evt_tx, event_rx) = mpsc::unbounded_channel();
        let (client_tx, client_rx) = mpsc::unbounded_channel();
        let accounting = Arc::new(Accounting::new(
            AccountingConfig::default(),
            test_identity(),
        ));
        let svc = PseudosettleService::new(command_rx, event_rx, client_tx, accounting);
        (svc, client_rx)
    }

    // Deliver a creditor ack carrying `timestamp` (Unix seconds) for a settle we
    // had pending. Returns once the `Sent` handler has updated the rate gate.
    async fn receive_ack(svc: &mut TestService, peer: OverlayAddress, timestamp: i64) {
        let _rx = insert_pending(svc, peer, Au::from_amount(100));
        svc.handle_event(PseudosettleEvent::Sent {
            peer,
            ack: PseudosettleAck {
                accepted: Au::from_amount(1),
                timestamp,
            },
        })
        .await;
    }

    // Attempt an outbound settle and report whether the rate gate allowed it.
    // `true` means the settle was sent (entry pending an ack, command emitted);
    // `false` means it was refused as `TooSoon`. The pending entry is cleared so
    // a follow-up settle is not rejected as `SettlementInProgress`.
    async fn settle_allowed(
        svc: &mut TestService,
        client_rx: &mut mpsc::UnboundedReceiver<ClientCommand>,
        peer: OverlayAddress,
    ) -> bool {
        let (response_tx, mut response_rx) = oneshot::channel();
        svc.handle_command(PseudosettleCommand::Settle {
            peer,
            amount: Au::from_amount(1),
            response_tx,
        })
        .await;
        match response_rx.try_recv() {
            Ok(Err(PseudosettleSettlementError::TooSoon)) => false,
            Err(oneshot::error::TryRecvError::Empty) => {
                // Passed the gate: a command was emitted and the settle now
                // awaits its ack. Reset so the next settle starts fresh.
                assert!(matches!(
                    client_rx.try_recv(),
                    Ok(ClientCommand::Peer {
                        command: PeerCommand::SendPseudosettle { .. },
                        ..
                    })
                ));
                svc.pending.remove(&peer);
                true
            }
            other => panic!("unexpected settle outcome: {other:?}"),
        }
    }

    #[tokio::test]
    async fn legit_ack_timestamp_gates_next_settle() {
        let (mut svc, mut rx) = build_service_with_rx();

        // A creditor ack one second in the past: the ack timestamp itself (not
        // our send clock) becomes the reference, and enough wall-clock has
        // elapsed, so the next settle is sent.
        let peer = peer_n(1);
        let now = current_timestamp();
        receive_ack(&mut svc, peer, now as i64 - 1).await;
        assert_eq!(*svc.last_settle_ack.get(&peer).unwrap(), now - 1);
        assert!(settle_allowed(&mut svc, &mut rx, peer).await);

        // A reference that has not yet elapsed (the creditor's clock at or
        // ahead of ours) holds the next settle off as `TooSoon`. The
        // future-clamp test proves a peer cannot push the reference past our
        // clock through the ack; here the gate itself is exercised.
        let peer2 = peer_n(2);
        svc.last_settle_ack.insert(peer2, current_timestamp() + 1);
        assert!(!settle_allowed(&mut svc, &mut rx, peer2).await);
    }

    #[tokio::test]
    async fn future_ack_timestamp_does_not_freeze_settling() {
        let (mut svc, mut rx) = build_service_with_rx();
        let peer = test_peer();

        // A creditor reports a timestamp far in the future. Stored verbatim it
        // would push the gate beyond reach and freeze our settling; the clamp
        // pins the reference to our own clock instead.
        let before = current_timestamp();
        receive_ack(&mut svc, peer, before as i64 + 10_000).await;
        let after = current_timestamp();

        let stored = *svc.last_settle_ack.get(&peer).unwrap();
        assert!(
            (before..=after).contains(&stored),
            "future ack timestamp must be clamped to our clock, got {stored}"
        );

        // Because the reference is our clock (not the future), one second of
        // real elapsed time unblocks the next settle. Emulate that second
        // elapsing by reading the gate against a reference one second old.
        svc.last_settle_ack.insert(peer, stored.saturating_sub(1));
        assert!(settle_allowed(&mut svc, &mut rx, peer).await);
    }

    #[tokio::test]
    async fn garbage_ack_timestamp_falls_back_to_now() {
        let (mut svc, _rx) = build_service_with_rx();
        let before = current_timestamp();

        // A nanoseconds-scale value (a hostile peer can send anything): far
        // past `now`, rejected.
        let nanos = peer_n(1);
        receive_ack(&mut svc, nanos, 1_700_000_000_000_000_000).await;

        // A negative timestamp falls back to `now`.
        let negative = peer_n(2);
        receive_ack(&mut svc, negative, -5).await;

        // Zero falls back to `now`.
        let zero = peer_n(3);
        receive_ack(&mut svc, zero, 0).await;

        // Each garbage value paces normally: the reference is our own clock at
        // ingestion, never the attacker-supplied number.
        let after = current_timestamp();
        for peer in [nanos, negative, zero] {
            let stored = *svc.last_settle_ack.get(&peer).unwrap();
            assert!(
                (before..=after).contains(&stored),
                "garbage ack timestamp must fall back to our clock, got {stored}"
            );
        }
    }

    #[tokio::test]
    async fn ack_timestamp_reference_is_monotonic() {
        let (mut svc, _rx) = build_service_with_rx();
        let peer = test_peer();

        // A recent ack sets the reference.
        let now = current_timestamp();
        receive_ack(&mut svc, peer, now as i64).await;
        assert_eq!(*svc.last_settle_ack.get(&peer).unwrap(), now);

        // A later ack reporting an older timestamp must not rewind the gate,
        // which would otherwise let a creditor induce settle spam.
        receive_ack(&mut svc, peer, now as i64 - 5).await;
        assert_eq!(*svc.last_settle_ack.get(&peer).unwrap(), now);
    }

    #[tokio::test]
    async fn first_contact_settle_is_allowed() {
        let (mut svc, mut rx) = build_service_with_rx();
        let peer = test_peer();

        // No prior ack: nothing in the rate gate, so the first settle is sent.
        assert!(!svc.last_settle_ack.contains_key(&peer));
        assert!(settle_allowed(&mut svc, &mut rx, peer).await);
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

    // The peer owes us far more than any plausible time-based allowance, so the
    // only thing capping a grant is `allowance_rate * elapsed`. The peer is
    // connected as a storer so its allowance is the full base rate. Returns the
    // service with the owed balance recorded.
    fn service_with_large_debt(peer: OverlayAddress) -> TestService {
        let svc = build_service();
        svc.accounting.connect_peer(peer, SwarmNodeType::Storer);
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
        let mut svc = service_with_large_debt(peer);

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
        let mut svc = service_with_large_debt(peer);

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

    #[test]
    fn missing_anchor_yields_zero_allowance() {
        let peer = test_peer();
        let svc = service_with_large_debt(peer);

        // Neither anchor map holds this peer, so the zero provably comes from
        // the missing-anchor branch rather than the positive-balance guard.
        assert!(!svc.last_settlement.contains_key(&peer));
        assert!(!svc.first_seen.contains_key(&peer));

        let handle = svc.accounting.for_peer(peer);
        let requested = Au::from_amount(1_000_000_000_000);
        // Pins the missing-anchor branch to a hard zero grant. A re-arming
        // seeded from the absolute epoch, or any positive grant reaching this
        // branch, trips the assertion; a `now` seed also yields zero elapsed,
        // so that shape is not distinguished here.
        assert_eq!(
            svc.calculate_acceptable(&peer, &handle, requested),
            Au::ZERO
        );
    }

    #[test]
    fn present_anchor_allowance_arithmetic_unchanged() {
        let peer = test_peer();
        let refresh_rate = Au::from_amount(4_500_000);
        let mut svc = service_with_large_debt(peer);

        // A settlement anchored ten seconds ago: the normal path computes
        // refresh_rate * elapsed, so the grant tracks ten times the rate.
        let now = current_timestamp();
        svc.last_settlement.insert(peer, now - 10);

        let handle = svc.accounting.for_peer(peer);
        let acceptable =
            svc.calculate_acceptable(&peer, &handle, Au::from_amount(1_000_000_000_000));

        let floor = refresh_rate.checked_scale(10).unwrap();
        let ceiling = refresh_rate.checked_scale(12).unwrap();
        assert!(
            acceptable >= floor && acceptable <= ceiling,
            "present-anchor grant {acceptable} left the expected [{floor}, {ceiling}] band"
        );
    }

    // Drain the single expected inbound ack from the outbound command channel,
    // asserting exactly one ack per refreshment (the single-consumer contract).
    fn drain_single_ack(rx: &mut mpsc::UnboundedReceiver<ClientCommand>) -> PseudosettleAck {
        let ack = match rx.try_recv() {
            Ok(ClientCommand::Peer {
                command: PeerCommand::AckPseudosettle { ack, .. },
                ..
            }) => ack,
            other => panic!("expected exactly one pseudosettle ack, got {other:?}"),
        };
        assert!(
            rx.try_recv().is_err(),
            "an inbound refreshment must produce exactly one ack"
        );
        ack
    }

    #[tokio::test]
    async fn allowance_is_keyed_on_the_remote_node_type() {
        // Default config: base refresh 4_500_000, client factor 10. A storer
        // remote accrues the full base rate, a client remote the scaled rate,
        // from one service on one node.
        let svc = build_service();
        let storer = peer_n(1);
        let client = peer_n(2);
        svc.accounting.connect_peer(storer, SwarmNodeType::Storer);
        svc.accounting.connect_peer(client, SwarmNodeType::Client);

        let mut svc = svc;
        let now = current_timestamp();
        for peer in [storer, client] {
            svc.accounting
                .for_peer(peer)
                .record(Au::from_amount(1_000_000_000_000), Direction::Upload);
            svc.last_settlement.insert(peer, now - 10);
        }

        let requested = Au::from_amount(1_000_000_000_000);
        let storer_handle = svc.accounting.for_peer(storer);
        let storer_grant = svc.calculate_acceptable(&storer, &storer_handle, requested);
        let client_handle = svc.accounting.for_peer(client);
        let client_grant = svc.calculate_acceptable(&client, &client_handle, requested);

        let base = Au::from_amount(4_500_000);
        let scaled = Au::from_amount(450_000);
        assert!(storer_grant >= base.checked_scale(10).unwrap());
        assert!(storer_grant <= base.checked_scale(12).unwrap());
        assert!(client_grant >= scaled.checked_scale(10).unwrap());
        assert!(client_grant <= scaled.checked_scale(12).unwrap());
    }

    #[tokio::test]
    async fn unconnected_peer_accrues_the_client_allowance() {
        // A peer whose handshake type never arrived keeps the lazy client-rate
        // seed, the conservative default against allowance over-grant.
        let mut svc = build_service();
        let peer = test_peer();
        svc.accounting
            .for_peer(peer)
            .record(Au::from_amount(1_000_000_000_000), Direction::Upload);
        svc.last_settlement.insert(peer, current_timestamp() - 10);

        let handle = svc.accounting.for_peer(peer);
        let grant = svc.calculate_acceptable(&peer, &handle, Au::from_amount(1_000_000_000_000));

        let scaled = Au::from_amount(450_000);
        assert!(grant >= scaled.checked_scale(10).unwrap());
        assert!(grant <= scaled.checked_scale(12).unwrap());
    }

    #[tokio::test]
    async fn inbound_over_claim_is_clamped_acked_credited_and_accumulated_consistently() {
        // One refreshment: the wire ack, the ledger credit, and the repayment
        // accumulator must all carry the same clamped amount, never the claim.
        let (svc, mut rx) = build_service_with_rx();
        let peer = test_peer();
        svc.accounting.connect_peer(peer, SwarmNodeType::Storer);
        let owed = Au::from_amount(1_000_000_000_000);
        svc.accounting
            .for_peer(peer)
            .record(owed, Direction::Upload);

        let mut svc = svc;
        svc.last_settlement.insert(peer, current_timestamp() - 10);
        svc.handle_event(PseudosettleEvent::Received {
            peer,
            amount: U256::from(u128::MAX),
            request_id: 7,
        })
        .await;

        let ack = drain_single_ack(&mut rx);
        let base = Au::from_amount(4_500_000);
        assert!(ack.accepted >= base.checked_scale(10).unwrap());
        assert!(ack.accepted <= base.checked_scale(12).unwrap());

        let state = svc.accounting.peer_state(peer);
        assert_eq!(state.balance(), owed - ack.accepted);
        assert_eq!(state.settlement_received(), ack.accepted);
    }

    #[tokio::test]
    async fn repayment_accumulates_across_refreshments() {
        let (svc, mut rx) = build_service_with_rx();
        let peer = test_peer();
        svc.accounting.connect_peer(peer, SwarmNodeType::Storer);
        svc.accounting
            .for_peer(peer)
            .record(Au::from_amount(1_000_000_000_000), Direction::Upload);

        let mut svc = svc;
        let mut total = Au::ZERO;
        for request_id in 0..2 {
            // Re-open the allowance window so the second refreshment is not
            // rate-gated to zero; each accepted amount must accumulate.
            svc.last_settlement.insert(peer, current_timestamp() - 10);
            svc.handle_event(PseudosettleEvent::Received {
                peer,
                amount: U256::from(1_000_000u64),
                request_id,
            })
            .await;
            let ack = drain_single_ack(&mut rx);
            assert_eq!(ack.accepted, Au::from_amount(1_000_000));
            total += ack.accepted;
            assert_eq!(svc.accounting.peer_state(peer).settlement_received(), total);
        }
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
    async fn failed_event_releases_pending_settle() {
        let mut svc = build_service();
        let peer = test_peer();

        // A settle is pending its ack when the substream dies.
        let mut rx = insert_pending(&mut svc, peer, Au::from_amount(100));
        svc.handle_event(PseudosettleEvent::Failed { peer }).await;

        // The caller is released with an error rather than left hanging.
        assert!(matches!(
            rx.try_recv().unwrap(),
            Err(PseudosettleSettlementError::NetworkError(_))
        ));
        // The pending entry is cleared so a later settle starts fresh.
        assert!(!svc.pending.contains_key(&peer));
    }

    #[tokio::test]
    async fn failed_event_for_unknown_peer_is_noop() {
        let mut svc = build_service();
        // No pending settle for this peer: the failure is simply ignored.
        svc.handle_event(PseudosettleEvent::Failed { peer: test_peer() })
            .await;
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
