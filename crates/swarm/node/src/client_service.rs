//! Client service bridging business logic and the network layer.
//!
//! Owns channels to `ClientBehaviour` and processes incoming events.

use std::sync::Arc;

use nectar_primitives::ChunkAddress;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};
use vertex_swarm_api::{
    Admission, AdmissionControl, Au, BandwidthDebit, PeerReporter, ReportSource, SwarmLocalStore,
    SwarmPricing, SwarmScoringEvent,
};
use vertex_swarm_client_protocol::PseudosettleAck;
pub use vertex_swarm_client_protocol::{ChunkTransferError, RetrievalResult};
use vertex_swarm_net_pushsync::Receipt;
use vertex_swarm_primitives::{CachedChunk, OverlayAddress, StampedChunk};
use vertex_tasks::{GracefulShutdown, MaybeSend, SpawnableTask};

use crate::inflight::PeerInflightLimiter;
use crate::protocol::{ClientCommand, ClientEvent, FailureKind};
use crate::retrieval_latency::RetrievalLatency;
use crate::selection::SettlementTrigger;

const RETRIEVAL_SOURCE: ReportSource = ReportSource::Protocol("retrieval");
const PUSHSYNC_SOURCE: ReportSource = ReportSource::Protocol("pushsync");

pub(crate) const DEFAULT_CHANNEL_CAPACITY: usize = 256;

/// Handle for sending commands to the network layer.
///
/// Request methods ([`Self::retrieve_chunk`], [`Self::push_chunk`]) thread a
/// response channel through the command into the per-connection handler, so each
/// outbound request is a self-contained future correlated by its substream with
/// no shared rendezvous state. Concurrent requests for the same chunk address
/// never collide, so callers may race the same address across peers.
#[derive(Clone)]
pub struct ClientHandle {
    command_tx: mpsc::Sender<ClientCommand>,
    /// When set, an origin request gates on the admission band and books its
    /// price the moment it dispatches, refunding only when the request provably
    /// reached no charge. Absent on the lightweight launcher, where origin
    /// dispatch neither books nor gates.
    origin: Option<OriginGate>,
}

/// Book-at-send and the admission band for origin requests.
///
/// All four read the one shared accounting the selector uses, so the dispatch
/// gate, the candidate selector, and the committed debit agree on one ledger.
/// The settlement trigger is the selector's, so settles dedup across both
/// paths. The band is the synchronous pacing brake: an over-threshold projected
/// debt settles or refuses before any bytes leave.
#[derive(Clone)]
struct OriginGate {
    pricing: Arc<dyn SwarmPricing>,
    debit: Arc<dyn BandwidthDebit>,
    admission: Arc<dyn AdmissionControl>,
    settlement: Arc<dyn SettlementTrigger>,
}

impl ClientHandle {
    /// Create a handle without an origin credit gate.
    pub fn new(command_tx: mpsc::Sender<ClientCommand>) -> Self {
        Self {
            command_tx,
            origin: None,
        }
    }

    /// Attach the origin credit gate so an own-request dispatch bands the
    /// request and books its price at the moment it dispatches.
    ///
    /// `pricing`, `debit`, and `admission` must read the one shared accounting
    /// the selector uses, and `settlement` must be the selector's so the
    /// in-flight settle dedup is shared. Relay legs (`originated == false`) are
    /// accounted by the forwarder and bypass this gate.
    #[must_use]
    pub fn with_origin_gate(
        mut self,
        pricing: Arc<dyn SwarmPricing>,
        debit: Arc<dyn BandwidthDebit>,
        admission: Arc<dyn AdmissionControl>,
        settlement: Arc<dyn SettlementTrigger>,
    ) -> Self {
        self.origin = Some(OriginGate {
            pricing,
            debit,
            admission,
            settlement,
        });
        self
    }

    /// Gate an origin request and book its price at dispatch.
    ///
    /// Returns the committed price for a possible later refund (`Ok(Some(_))`),
    /// or `Ok(None)` for a relay leg or when no origin gate is attached. An
    /// [`Admit`](Admission::Admit) band books and sends; a
    /// [`SettleAndAdmit`](Admission::SettleAndAdmit) triggers a settle and sends
    /// anyway (the band keeps us under the disconnect line whichever order the
    /// storer applies them); a [`Refuse`](Admission::Refuse) at the disconnect
    /// line triggers a settle and refuses, so the caller routes elsewhere.
    ///
    /// Booking happens the moment the request dispatches, before any await, and
    /// is refunded only when the request provably reaches no charge. Every chunk
    /// the server serves thus corresponds to a request we already committed, so
    /// our debt-view stays at or above the server's and our band refuses before
    /// the server's disconnect line. A losing race leg dropped mid-flight cannot
    /// un-book, because the commit already happened synchronously here.
    fn reserve_origin(
        &self,
        peer: OverlayAddress,
        address: &ChunkAddress,
        originated: bool,
    ) -> Result<Option<Au>, ChunkTransferError> {
        let Some(gate) = &self.origin else {
            return Ok(None);
        };
        if !originated {
            return Ok(None);
        }

        let price = gate.pricing.peer_price(&peer, address);
        match gate.admission.admit(&peer, price) {
            Admission::Refuse => {
                gate.settlement.trigger_settlement(peer);
                return Err(ChunkTransferError::Refused);
            }
            admission => {
                if admission.settles() {
                    gate.settlement.trigger_settlement(peer);
                }
            }
        }

        // The debit shares the admit boundary, so it refuses at the disconnect
        // line too; a concurrent burst that crossed it between the band check and
        // here surfaces as a refusal, handled identically. The commit lands
        // immediately at dispatch.
        match gate.debit.debit_received(peer, price, true) {
            Ok(()) => Ok(Some(price)),
            Err(_) => {
                gate.settlement.trigger_settlement(peer);
                Err(ChunkTransferError::Refused)
            }
        }
    }

    /// Refund a dispatch-committed origin debit. A no-op for a relay leg or when
    /// no origin gate is attached. Pure ledger op, never peer scoring.
    fn refund_origin(&self, peer: OverlayAddress, committed: Option<Au>) {
        if let (Some(gate), Some(price)) = (&self.origin, committed) {
            gate.debit.refund_received(peer, price);
        }
    }

    /// Send a command to the network layer.
    ///
    /// Non-blocking `try_send`: callers such as the libp2p event loop must not
    /// block.
    pub fn send_command(&self, command: ClientCommand) -> Result<(), ChunkTransferError> {
        self.command_tx.try_send(command).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => {
                warn!("Client command channel full");
                metrics::counter!("swarm.client.commands_dropped").increment(1);
                ChunkTransferError::ChannelClosed
            }
            mpsc::error::TrySendError::Closed(_) => ChunkTransferError::ChannelClosed,
        })
    }

    /// Retrieve a chunk from a specific peer.
    ///
    /// Any failure on the path resolves or drops the response channel, so this
    /// future never hangs. `originated` is `true` for our own request and
    /// `false` for a forwarder relay leg; it travels to the completion event so
    /// only origin requests are debited (the forwarder accounts its own legs).
    pub async fn retrieve_chunk(
        &self,
        peer: OverlayAddress,
        address: ChunkAddress,
        originated: bool,
    ) -> Result<RetrievalResult, ChunkTransferError> {
        // Gate on the band and book the price at dispatch.
        let committed = self.reserve_origin(peer, &address, originated)?;

        let (tx, rx) = oneshot::channel();

        if let Err(e) = self.send_command(ClientCommand::RetrieveChunk {
            peer,
            address,
            response: tx,
            originated,
        }) {
            // Never reached the wire, so nothing was charged: refund.
            self.refund_origin(peer, committed);
            return Err(e);
        }

        // A dropped response oneshot is a mid-flight teardown (`Cancelled`), not a
        // confirmed absence, so the dispatch commit stays like any lost delivery.
        let result = rx.await.unwrap_or(Err(ChunkTransferError::Cancelled));
        if let Err(e) = &result
            && e.is_confirmed_absent()
        {
            self.refund_origin(peer, committed);
        }
        result
    }

    /// Push a stamped chunk to a specific peer.
    ///
    /// Same failure semantics as [`Self::retrieve_chunk`]. The returned
    /// [`Receipt`] is storer-verified at the decode boundary, so an `Ok` here
    /// always carries a recovered storer. `originated` distinguishes our own
    /// push from a forwarder relay leg.
    pub async fn push_chunk(
        &self,
        peer: OverlayAddress,
        chunk: StampedChunk,
        originated: bool,
    ) -> Result<Receipt, ChunkTransferError> {
        let address = *chunk.address();

        // Pushsync gates and books at dispatch like retrieval.
        let committed = self.reserve_origin(peer, &address, originated)?;

        let (tx, rx) = oneshot::channel();

        if let Err(e) = self.send_command(ClientCommand::PushChunk {
            peer,
            address,
            chunk,
            response: tx,
            originated,
        }) {
            self.refund_origin(peer, committed);
            return Err(e);
        }

        let result = rx.await.unwrap_or(Err(ChunkTransferError::Cancelled));
        if let Err(e) = &result
            && e.is_confirmed_absent()
        {
            self.refund_origin(peer, committed);
        }
        result
    }
}

/// Business-logic layer that processes `ClientEvent`s from the network.
pub struct ClientService {
    handle: ClientHandle,
    event_rx: mpsc::Receiver<ClientEvent>,
    /// Peer scoring authority fed by retrieval and pushsync outcomes.
    /// Best-effort: without it, outcomes only surface as logs.
    reporter: Option<Arc<dyn PeerReporter>>,
    /// Client cache for the node's own retrieval deliveries. Content chunks are
    /// cached; single-owner chunks are not (no version signal).
    store: Option<Arc<dyn SwarmLocalStore>>,
    /// Per-peer retrieval in-flight limiter shared with the chunk provider;
    /// the peer entry is forgotten on disconnect.
    inflight: Option<Arc<PeerInflightLimiter>>,
    /// Per-PO retrieval-latency estimate shared with the chunk provider; a
    /// completed originated retrieval is recorded here keyed by its proximity.
    retrieval_latency: Option<Arc<RetrievalLatency>>,
}

impl ClientService {
    /// Create a service with default channel capacity, returning the service, an
    /// event sender for the network layer, and a command handle.
    pub fn new() -> (Self, mpsc::Sender<ClientEvent>, ClientHandle) {
        let (command_tx, _command_rx) = mpsc::channel(DEFAULT_CHANNEL_CAPACITY);
        let (event_tx, event_rx) = mpsc::channel(DEFAULT_CHANNEL_CAPACITY);

        let handle = ClientHandle::new(command_tx);

        let service = Self {
            handle: handle.clone(),
            event_rx,
            reporter: None,
            store: None,
            inflight: None,
            retrieval_latency: None,
        };

        (service, event_tx, handle)
    }

    /// Create with explicit channels, for when the network layer owns the
    /// command channel.
    pub fn with_channels(
        command_tx: mpsc::Sender<ClientCommand>,
        event_rx: mpsc::Receiver<ClientEvent>,
    ) -> (Self, ClientHandle) {
        let handle = ClientHandle::new(command_tx);

        let service = Self {
            handle: handle.clone(),
            event_rx,
            reporter: None,
            store: None,
            inflight: None,
            retrieval_latency: None,
        };

        (service, handle)
    }

    /// Attach a peer reporter so retrieval and pushsync outcomes feed scoring.
    #[must_use]
    pub fn with_reporter(mut self, reporter: Arc<dyn PeerReporter>) -> Self {
        self.reporter = Some(reporter);
        self
    }

    /// Attach the client cache so the service caches its own retrieval
    /// deliveries.
    ///
    /// Content chunks are cached by address (immutable); single-owner chunks are
    /// not, since a stampless SOC has no version signal and could serve a stale
    /// revision.
    #[must_use]
    pub fn with_store(mut self, store: Arc<dyn SwarmLocalStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// Attach the per-peer retrieval in-flight limiter so the service forgets a
    /// peer's slot accounting on disconnect.
    ///
    /// Must be the same [`PeerInflightLimiter`] the chunk provider reserves
    /// against via [`NetworkChunkProvider::with_inflight_limiter`].
    ///
    /// [`NetworkChunkProvider::with_inflight_limiter`]: crate::NetworkChunkProvider::with_inflight_limiter
    #[must_use]
    pub fn with_inflight_limiter(mut self, inflight: Arc<PeerInflightLimiter>) -> Self {
        self.inflight = Some(inflight);
        self
    }

    /// Attach the per-PO retrieval-latency estimate so a completed originated
    /// retrieval feeds the hedge the chunk provider paces its race with.
    ///
    /// Must be the same [`RetrievalLatency`] the chunk provider reads via
    /// `NetworkChunkProvider::with_retrieval_latency`.
    #[must_use]
    pub(crate) fn with_retrieval_latency(mut self, latency: Arc<RetrievalLatency>) -> Self {
        self.retrieval_latency = Some(latency);
        self
    }

    /// Get a handle for sending commands.
    pub fn handle(&self) -> ClientHandle {
        self.handle.clone()
    }

    fn report(&self, peer: &OverlayAddress, event: SwarmScoringEvent, source: ReportSource) {
        if let Some(reporter) = &self.reporter {
            reporter.report_peer(peer, event, source);
        }
    }

    /// Run the event processing loop with graceful shutdown support.
    pub async fn run(mut self, shutdown: GracefulShutdown) {
        let mut shutdown = std::pin::pin!(shutdown);

        loop {
            tokio::select! {
                guard = &mut shutdown => {
                    debug!("Client service received shutdown signal");
                    drop(guard);
                    break;
                }
                event = self.event_rx.recv() => {
                    match event {
                        Some(event) => self.process_event(event),
                        None => {
                            debug!("Client service event channel closed");
                            break;
                        }
                    }
                }
            }
        }
        debug!("Client service shutdown complete");
    }

    /// Process a single event.
    fn process_event(&self, event: ClientEvent) {
        match event {
            ClientEvent::PeerActivated { peer_id, overlay } => {
                debug!(%peer_id, %overlay, "Peer activated for client protocols");
                // TODO: Trigger pricing announcement based on peer type
            }

            ClientEvent::PricingReceived {
                peer,
                peer_id,
                threshold,
            } => {
                debug!(%peer_id, %peer, %threshold, "Received pricing threshold");
                // TODO: Validate threshold against minimum
                // TODO: Store peer's threshold for bandwidth accounting
            }

            ClientEvent::PricingSent { peer } => {
                debug!(%peer, "Pricing threshold sent");
            }

            ClientEvent::ChunkReceived {
                peer,
                address,
                chunk,
                stamp,
                latency,
                originated,
            } => {
                // The requester is resolved by the handler; this event exists for
                // scoring and caching. Content chunks are cached by address
                // (immutable); SOCs are not (no version signal). The origin debit
                // is committed by the dispatch reservation, not here; a relay leg
                // is accounted by the forwarder. Cache and scoring apply to every
                // delivery.
                debug!(%peer, %address, ?latency, "Chunk received");
                // Feed the per-PO latency estimate so the chunk provider can pace
                // its staggered race to the forwarding distance. Only originated
                // retrievals: a relay leg's latency is the requester's chain, not
                // ours. Keyed by PO(serving_peer, chunk), the forwarding distance.
                if originated && let Some(latency_estimate) = &self.retrieval_latency {
                    latency_estimate.record(address.proximity(&peer).get(), latency);
                }
                if let Some(store) = &self.store
                    && chunk.is_content()
                {
                    let _ = store.put(CachedChunk::new(chunk, stamp));
                }
                self.report(
                    &peer,
                    SwarmScoringEvent::RetrievalSuccess { latency },
                    RETRIEVAL_SOURCE,
                );
            }

            ClientEvent::InboundServed { peer } => {
                debug!(%peer, "Served inbound retrieval from cache");
                metrics::counter!("swarm.client.inbound_served").increment(1);
            }

            ClientEvent::InboundForwarded { peer } => {
                debug!(%peer, "Forwarded inbound retrieval to a closer peer");
                metrics::counter!("swarm.client.inbound_forwarded").increment(1);
            }

            ClientEvent::InboundMissed { peer, address } => {
                debug!(%peer, %address, "Inbound retrieval missed (substream reset)");
                metrics::counter!("swarm.client.inbound_missed").increment(1);
            }

            ClientEvent::InboundRelayed { peer } => {
                debug!(%peer, "Relayed pushsync receipt to pusher");
                metrics::counter!("swarm.client.inbound_relayed").increment(1);
            }

            ClientEvent::InboundStored { peer } => {
                debug!(%peer, "Stored inbound pushsync delivery and signed a receipt");
                metrics::counter!("swarm.client.inbound_stored").increment(1);
            }

            ClientEvent::InboundPushFailed { peer, address } => {
                debug!(%peer, %address, "Inbound pushsync failed (substream reset)");
                metrics::counter!("swarm.client.inbound_push_failed").increment(1);
            }

            ClientEvent::ReceiptReceived {
                peer,
                address,
                latency,
                originated: _,
            } => {
                // The pusher is resolved by the handler; this event exists for
                // scoring. The origin debit is committed by the dispatch
                // reservation, not here; a relay leg is accounted by the
                // forwarder.
                debug!(%peer, %address, ?latency, "Receipt received");
                self.report(
                    &peer,
                    SwarmScoringEvent::PushSuccess { latency },
                    PUSHSYNC_SOURCE,
                );
            }

            ClientEvent::PeerDisconnected { peer_id, overlay } => {
                debug!(%peer_id, %overlay, "Peer disconnected");
                if let Some(inflight) = &self.inflight {
                    inflight.forget(&overlay);
                }
            }

            ClientEvent::ProtocolError {
                peer,
                peer_id,
                protocol,
                error,
            } => {
                warn!(
                    peer = ?peer,
                    peer_id = ?peer_id,
                    %protocol,
                    %error,
                    "Protocol error"
                );
                // TODO: Handle specific errors, maybe disconnect peer
            }

            ClientEvent::RetrievalFailed {
                peer,
                address,
                error,
                kind,
            } => {
                // Scoring policy: a malformed chunk is misbehaviour and scored
                // adversely. A plain `Protocol` failure (miss, timeout, or
                // transport error) is blameless and not scored, so a bulk
                // download's flood of misses cannot decay the peer set past the
                // disconnect threshold; the staggered race steers around an
                // unhelpful candidate within a request instead.
                warn!(%peer, %address, %error, ?kind, "Retrieval failed");
                match kind {
                    FailureKind::InvalidChunk => {
                        metrics::counter!(
                            "swarm.client.invalid_chunk",
                            "protocol" => "retrieval",
                        )
                        .increment(1);
                        self.report(&peer, SwarmScoringEvent::InvalidData, RETRIEVAL_SOURCE);
                    }
                    FailureKind::Protocol => {
                        // Blameless miss: counted but not scored.
                        metrics::counter!(
                            "swarm.client.retrieval_miss",
                            "protocol" => "retrieval",
                        )
                        .increment(1);
                    }
                }
            }

            ClientEvent::PushFailed {
                peer,
                address,
                error,
                kind,
            } => {
                // Same scoring policy as retrieval: a malformed receipt is
                // scored, a plain `Protocol` failure is blameless.
                warn!(%peer, %address, %error, ?kind, "Push failed");
                match kind {
                    FailureKind::InvalidChunk => {
                        metrics::counter!(
                            "swarm.client.invalid_chunk",
                            "protocol" => "pushsync",
                        )
                        .increment(1);
                        self.report(&peer, SwarmScoringEvent::InvalidData, PUSHSYNC_SOURCE);
                    }
                    FailureKind::Protocol => {
                        metrics::counter!(
                            "swarm.client.retrieval_miss",
                            "protocol" => "pushsync",
                        )
                        .increment(1);
                    }
                }
            }

            ClientEvent::InboundInvalidData { peer, protocol } => {
                // Decode rejected a malformed inbound chunk or request before
                // relay; score the sender adversely.
                warn!(%peer, %protocol, "Inbound malformed data rejected");
                metrics::counter!(
                    "swarm.client.invalid_chunk",
                    "protocol" => protocol,
                )
                .increment(1);
                self.report(
                    &peer,
                    SwarmScoringEvent::InvalidData,
                    ReportSource::Protocol(protocol),
                );
            }

            ClientEvent::PseudosettleReceived {
                peer,
                peer_id,
                amount,
                request_id,
            } => {
                debug!(%peer, %peer_id, %amount, %request_id, "Pseudosettle received");

                // TODO: Validate amount against accounting rules
                // TODO: Credit peer's balance in accounting system:
                //   accounting.for_peer(peer).credit(amount.as_u64() as i64);

                let ack = PseudosettleAck {
                    accepted: Au::saturating_from_u256(amount),
                    // Unix seconds: the payer rejects an ack whose timestamp is
                    // more than a couple of seconds off its own clock.
                    timestamp: vertex_util_runtime::time::now_unix_secs() as i64,
                };

                if let Err(e) = self.handle.send_command(ClientCommand::AckPseudosettle {
                    peer,
                    request_id,
                    ack,
                }) {
                    warn!(%peer, %peer_id, error = ?e, "Failed to send pseudosettle ack");
                }
            }

            ClientEvent::PseudosettleSent { peer, peer_id, ack } => {
                debug!(%peer, %peer_id, amount = %ack.accepted, timestamp = ack.timestamp, "Pseudosettle sent, received ack");
            }

            #[cfg(feature = "swap")]
            ClientEvent::SwapChequeReceived {
                peer,
                peer_id,
                peer_rate,
                ..
            } => {
                // The swap settlement service consumes cheques via the dedicated
                // channel configured with `route_swap_events`.
                debug!(%peer, %peer_id, %peer_rate, "Swap cheque received");
            }

            #[cfg(feature = "swap")]
            ClientEvent::SwapChequeSent {
                peer,
                peer_id,
                peer_rate,
            } => {
                debug!(%peer, %peer_id, %peer_rate, "Swap cheque sent");
            }
            // `ClientEvent` carries swap variants when `client-protocol/swap`
            // is on, which Cargo feature unification can turn on (a workspace
            // build also compiling `accounting-swap`) even when this crate's
            // `swap` feature is off. The swap wire is then not linked here, so
            // ignore them. The all-features build keeps full exhaustiveness.
            // Unreachable when nothing in the build enables `client-protocol/swap`.
            #[cfg(not(feature = "swap"))]
            #[allow(unreachable_patterns)]
            _ => {}
        }
    }
}

impl Default for ClientService {
    fn default() -> Self {
        Self::new().0
    }
}

impl SpawnableTask for ClientService {
    fn into_task(
        self,
        shutdown: GracefulShutdown,
    ) -> impl std::future::Future<Output = ()> + MaybeSend {
        self.run(shutdown)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::Duration;

    use super::*;

    #[derive(Default)]
    struct RecordingReporter {
        reports: Mutex<Vec<(OverlayAddress, SwarmScoringEvent, ReportSource)>>,
    }

    impl PeerReporter for RecordingReporter {
        fn report_peer(
            &self,
            overlay: &OverlayAddress,
            event: SwarmScoringEvent,
            source: ReportSource,
        ) {
            self.reports.lock().unwrap().push((*overlay, event, source));
        }
    }

    impl RecordingReporter {
        fn single(&self) -> (OverlayAddress, SwarmScoringEvent, ReportSource) {
            let reports = self.reports.lock().unwrap();
            assert_eq!(reports.len(), 1, "expected exactly one report");
            *reports.first().expect("one report")
        }

        fn assert_none(&self) {
            let reports = self.reports.lock().unwrap();
            assert!(reports.is_empty(), "expected no report, got {reports:?}");
        }
    }

    fn peer(n: u8) -> OverlayAddress {
        OverlayAddress::from([n; 32])
    }

    fn service_with_reporter() -> (ClientService, Arc<RecordingReporter>) {
        let reporter = Arc::new(RecordingReporter::default());
        let (service, _event_tx, _handle) = ClientService::new();
        let service = service.with_reporter(Arc::clone(&reporter) as Arc<dyn PeerReporter>);
        (service, reporter)
    }

    #[test]
    fn malformed_retrieval_reports_invalid_data() {
        let (service, reporter) = service_with_reporter();
        service.process_event(ClientEvent::RetrievalFailed {
            peer: peer(1),
            address: ChunkAddress::zero(),
            error: "invalid chunk".into(),
            kind: FailureKind::InvalidChunk,
        });
        let (reported_peer, event, source) = reporter.single();
        assert_eq!(reported_peer, peer(1));
        assert_eq!(event, SwarmScoringEvent::InvalidData);
        assert_eq!(source, ReportSource::Protocol("retrieval"));
    }

    #[test]
    fn plain_retrieval_failure_does_not_penalise_peer() {
        // `FailureKind::Protocol` is a blameless miss and must not be scored.
        let (service, reporter) = service_with_reporter();
        service.process_event(ClientEvent::RetrievalFailed {
            peer: peer(2),
            address: ChunkAddress::zero(),
            error: "not found".into(),
            kind: FailureKind::Protocol,
        });
        reporter.assert_none();
    }

    #[test]
    fn malformed_push_reports_invalid_data() {
        let (service, reporter) = service_with_reporter();
        service.process_event(ClientEvent::PushFailed {
            peer: peer(3),
            address: ChunkAddress::zero(),
            error: "invalid chunk".into(),
            kind: FailureKind::InvalidChunk,
        });
        let (_, event, source) = reporter.single();
        assert_eq!(event, SwarmScoringEvent::InvalidData);
        assert_eq!(source, ReportSource::Protocol("pushsync"));
    }

    #[test]
    fn plain_push_failure_does_not_penalise_peer() {
        // Mirror of the retrieval case: a `FailureKind::Protocol` push failure
        // must not be scored.
        let (service, reporter) = service_with_reporter();
        service.process_event(ClientEvent::PushFailed {
            peer: peer(4),
            address: ChunkAddress::zero(),
            error: "rejected".into(),
            kind: FailureKind::Protocol,
        });
        reporter.assert_none();
    }

    #[test]
    fn inbound_malformed_delivery_reports_invalid_data_against_sender() {
        let (service, reporter) = service_with_reporter();
        service.process_event(ClientEvent::InboundInvalidData {
            peer: peer(5),
            protocol: "pushsync",
        });
        let (reported_peer, event, source) = reporter.single();
        assert_eq!(reported_peer, peer(5));
        assert_eq!(event, SwarmScoringEvent::InvalidData);
        assert_eq!(source, ReportSource::Protocol("pushsync"));
    }

    #[test]
    fn receipt_received_reports_push_success_with_latency() {
        let (service, reporter) = service_with_reporter();
        service.process_event(ClientEvent::ReceiptReceived {
            peer: peer(6),
            address: ChunkAddress::zero(),
            latency: Duration::from_millis(42),
            originated: false,
        });
        let (_, event, source) = reporter.single();
        assert_eq!(
            event,
            SwarmScoringEvent::PushSuccess {
                latency: Duration::from_millis(42)
            }
        );
        assert_eq!(source, ReportSource::Protocol("pushsync"));
    }

    fn content_chunk() -> nectar_primitives::AnyChunk {
        nectar_primitives::ContentChunk::new(&b"origin-reserve-test"[..])
            .expect("valid content chunk")
            .into()
    }

    // Reserve-at-dispatch lifecycle over a real `Accounting`. The origin gate on
    // the handle reserves the price before sending, commits it on delivery, and
    // releases it on every other exit.
    use vertex_swarm_accounting::{Accounting, FixedPricingConfig};

    /// Records the peers a settle was triggered for.
    #[derive(Default)]
    struct RecordingSettlement {
        triggered: std::sync::Mutex<Vec<OverlayAddress>>,
    }

    impl SettlementTrigger for RecordingSettlement {
        fn trigger_settlement(&self, peer: OverlayAddress) {
            self.triggered.lock().unwrap().push(peer);
        }

        fn settled(
            &self,
            _peers: &[OverlayAddress],
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + '_>> {
            Box::pin(std::future::ready(false))
        }
    }

    /// A pricer charging a fixed price for every peer and chunk.
    struct FixedPeerPricer(u64);

    impl SwarmPricing for FixedPeerPricer {
        fn price(&self, _chunk: &ChunkAddress) -> Au {
            Au::from_amount(self.0)
        }
        fn peer_price(&self, _peer: &OverlayAddress, _chunk: &ChunkAddress) -> Au {
            Au::from_amount(self.0)
        }
    }

    type GatedAccounting = Accounting<DefaultBandwidthConfig, MockIdentity>;

    /// Build a handle whose origin gate reserves against a real `Accounting`.
    /// The config bands at payment 1000 (settle trigger 400, floored at refresh
    /// 10) and disconnect 1250, so a `price` of 100 admits, 800 lands in the
    /// tolerance band, and 2000 refuses. Returns the handle, the shared
    /// accounting (to observe balance and reserved), the recording settle
    /// trigger, and the command receiver.
    fn gated_handle(
        price: u64,
    ) -> (
        ClientHandle,
        Arc<GatedAccounting>,
        Arc<RecordingSettlement>,
        mpsc::Receiver<ClientCommand>,
    ) {
        let config =
            DefaultBandwidthConfig::new(1000, 25, 10, 60, 1, FixedPricingConfig::default());
        let accounting = Arc::new(Accounting::new(config, MockIdentity::with_first_byte(0)));
        let settlement = Arc::new(RecordingSettlement::default());
        let (tx, rx) = mpsc::channel::<ClientCommand>(16);
        let handle = ClientHandle::new(tx).with_origin_gate(
            Arc::new(FixedPeerPricer(price)) as Arc<dyn SwarmPricing>,
            accounting.clone() as Arc<dyn BandwidthDebit>,
            accounting.clone() as Arc<dyn AdmissionControl>,
            settlement.clone() as Arc<dyn SettlementTrigger>,
        );
        (handle, accounting, settlement, rx)
    }

    #[tokio::test]
    async fn origin_dispatch_books_immediately_and_keeps_it_on_delivery() {
        let (handle, accounting, settlement, mut rx) = gated_handle(100);
        let peer = peer(1);

        let task = tokio::spawn({
            let handle = handle.clone();
            async move {
                handle
                    .retrieve_chunk(peer, ChunkAddress::zero(), true)
                    .await
            }
        });

        // Book-at-send: the moment the command dispatches the debit is committed,
        // so the balance is already debited and nothing is left reserved (this is
        // the distinguishing behaviour from the old reserve-only-at-dispatch).
        let response = match rx.recv().await.expect("dispatched") {
            ClientCommand::RetrieveChunk {
                peer: p, response, ..
            } => {
                assert_eq!(p, peer);
                response
            }
            other => panic!("unexpected command: {other:?}"),
        };
        assert_eq!(Ledger::balance(&*accounting, &peer), Au::new(-100));
        assert_eq!(Ledger::reserved(&*accounting, &peer), Au::ZERO);

        // Delivery keeps the dispatch commit: still debited once.
        response
            .send(Ok(RetrievalResult {
                chunk: content_chunk(),
                stamp: None,
                peer,
            }))
            .expect("receiver alive");
        task.await.unwrap().expect("delivery ok");
        assert_eq!(Ledger::balance(&*accounting, &peer), Au::new(-100));
        assert_eq!(Ledger::reserved(&*accounting, &peer), Au::ZERO);
        // An in-band Admit needs no settle.
        assert!(settlement.triggered.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn origin_dispatch_keeps_the_commit_on_lost_delivery() {
        // Book-at-send: a `Remote` failure (the substream returned a result, but
        // not a delivery) may have moved bytes the server charged for, so the
        // dispatch commit stays. The path fires no settle and no scoring: an
        // in-band admit needs no settle, and the handle holds no peer reporter, so
        // a reset driven by our own debt never docks the server.
        let (handle, accounting, settlement, mut rx) = gated_handle(100);
        let peer = peer(2);

        let task = tokio::spawn({
            let handle = handle.clone();
            async move {
                handle
                    .retrieve_chunk(peer, ChunkAddress::zero(), true)
                    .await
            }
        });
        let response = match rx.recv().await.expect("dispatched") {
            ClientCommand::RetrieveChunk { response, .. } => response,
            other => panic!("unexpected command: {other:?}"),
        };
        // Already committed at dispatch.
        assert_eq!(Ledger::balance(&*accounting, &peer), Au::new(-100));
        assert_eq!(Ledger::reserved(&*accounting, &peer), Au::ZERO);

        response
            .send(Err(ChunkTransferError::Remote))
            .expect("receiver alive");
        assert!(matches!(
            task.await.unwrap(),
            Err(ChunkTransferError::Remote)
        ));
        assert_eq!(Ledger::balance(&*accounting, &peer), Au::new(-100));
        assert_eq!(Ledger::reserved(&*accounting, &peer), Au::ZERO);
        assert!(settlement.triggered.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn origin_dispatch_keeps_the_commit_on_cancelled() {
        // The cancel path: the response oneshot is dropped without an answer, so
        // the request returns `Cancelled`. This is a mid-flight teardown, not a
        // confirmed absence, so the dispatch commit stays (bytes may have flowed).
        let (handle, accounting, _settlement, mut rx) = gated_handle(100);
        let peer = peer(3);

        let task = tokio::spawn({
            let handle = handle.clone();
            async move {
                handle
                    .retrieve_chunk(peer, ChunkAddress::zero(), true)
                    .await
            }
        });
        let response = match rx.recv().await.expect("dispatched") {
            ClientCommand::RetrieveChunk { response, .. } => response,
            other => panic!("unexpected command: {other:?}"),
        };
        assert_eq!(Ledger::balance(&*accounting, &peer), Au::new(-100));

        drop(response);
        assert!(matches!(
            task.await.unwrap(),
            Err(ChunkTransferError::Cancelled)
        ));
        assert_eq!(Ledger::reserved(&*accounting, &peer), Au::ZERO);
        assert_eq!(Ledger::balance(&*accounting, &peer), Au::new(-100));
    }

    #[tokio::test]
    async fn origin_dispatch_refunds_on_confirmed_absence() {
        // A peer that answers with an explicit not-found delivery did not charge,
        // so the dispatch commit is refunded and the balance returns to its
        // pre-dispatch value.
        let (handle, accounting, _settlement, mut rx) = gated_handle(100);
        let peer = peer(9);

        let task = tokio::spawn({
            let handle = handle.clone();
            async move {
                handle
                    .retrieve_chunk(peer, ChunkAddress::zero(), true)
                    .await
            }
        });
        let response = match rx.recv().await.expect("dispatched") {
            ClientCommand::RetrieveChunk { response, .. } => response,
            other => panic!("unexpected command: {other:?}"),
        };
        // Committed at dispatch, then refunded on the confirmed absence.
        assert_eq!(Ledger::balance(&*accounting, &peer), Au::new(-100));

        response
            .send(Err(ChunkTransferError::NotFound(ChunkAddress::zero())))
            .expect("receiver alive");
        assert!(matches!(
            task.await.unwrap(),
            Err(ChunkTransferError::NotFound(_))
        ));
        assert_eq!(Ledger::balance(&*accounting, &peer), Au::ZERO);
        assert_eq!(Ledger::reserved(&*accounting, &peer), Au::ZERO);
    }

    #[tokio::test]
    async fn origin_dispatch_refunds_when_peer_unreached() {
        // `NotConnected` means the request never reached a peer, so nothing was
        // charged and the dispatch commit is refunded.
        let (handle, accounting, _settlement, mut rx) = gated_handle(100);
        let peer = peer(10);

        let task = tokio::spawn({
            let handle = handle.clone();
            async move {
                handle
                    .retrieve_chunk(peer, ChunkAddress::zero(), true)
                    .await
            }
        });
        let response = match rx.recv().await.expect("dispatched") {
            ClientCommand::RetrieveChunk { response, .. } => response,
            other => panic!("unexpected command: {other:?}"),
        };
        assert_eq!(Ledger::balance(&*accounting, &peer), Au::new(-100));

        response
            .send(Err(ChunkTransferError::NotConnected))
            .expect("receiver alive");
        assert!(matches!(
            task.await.unwrap(),
            Err(ChunkTransferError::NotConnected)
        ));
        assert_eq!(Ledger::balance(&*accounting, &peer), Au::ZERO);
        assert_eq!(Ledger::reserved(&*accounting, &peer), Au::ZERO);
    }

    #[tokio::test]
    async fn origin_dispatch_in_the_band_still_sends_and_triggers_settle() {
        // A price landing the projected debt in the tolerance band sends anyway
        // (closeness preserved) and triggers a settle.
        let (handle, _accounting, settlement, mut rx) = gated_handle(800);
        let peer = peer(4);

        let task = tokio::spawn({
            let handle = handle.clone();
            async move {
                handle
                    .retrieve_chunk(peer, ChunkAddress::zero(), true)
                    .await
            }
        });
        let response = match rx.recv().await.expect("dispatched anyway in the band") {
            ClientCommand::RetrieveChunk { response, .. } => response,
            other => panic!("unexpected command: {other:?}"),
        };
        assert_eq!(*settlement.triggered.lock().unwrap(), vec![peer]);
        response.send(Err(ChunkTransferError::Remote)).ok();
        let _ = task.await;
    }

    #[tokio::test]
    async fn origin_dispatch_refused_at_the_disconnect_line_skips_and_settles() {
        // A price past the disconnect line refuses: no bytes are sent, nothing is
        // reserved, and a settle is triggered so the peer drains.
        let (handle, accounting, settlement, mut rx) = gated_handle(2000);
        let peer = peer(5);

        let outcome = handle
            .retrieve_chunk(peer, ChunkAddress::zero(), true)
            .await;
        assert!(matches!(outcome, Err(ChunkTransferError::Refused)));
        assert!(
            rx.try_recv().is_err(),
            "no bytes are sent to a refused peer"
        );
        assert_eq!(*settlement.triggered.lock().unwrap(), vec![peer]);
        assert_eq!(Ledger::reserved(&*accounting, &peer), Au::ZERO);
        assert_eq!(Ledger::balance(&*accounting, &peer), Au::ZERO);
    }

    #[tokio::test]
    async fn relay_leg_bypasses_the_origin_gate() {
        // A relay leg (`originated = false`) neither reserves nor settles; the
        // forwarder accounts its own legs.
        let (handle, accounting, settlement, mut rx) = gated_handle(100);
        let peer = peer(6);

        let task = tokio::spawn({
            let handle = handle.clone();
            async move {
                handle
                    .retrieve_chunk(peer, ChunkAddress::zero(), false)
                    .await
            }
        });
        let response = match rx.recv().await.expect("dispatched") {
            ClientCommand::RetrieveChunk {
                originated,
                response,
                ..
            } => {
                assert!(!originated, "a relay leg is not an origin request");
                response
            }
            other => panic!("unexpected command: {other:?}"),
        };
        assert_eq!(
            Ledger::reserved(&*accounting, &peer),
            Au::ZERO,
            "a relay leg holds no reservation"
        );
        response
            .send(Ok(RetrievalResult {
                chunk: content_chunk(),
                stamp: None,
                peer,
            }))
            .ok();
        let _ = task.await;
        assert_eq!(
            Ledger::balance(&*accounting, &peer),
            Au::ZERO,
            "a relay leg is not debited by the origin gate"
        );
        assert!(settlement.triggered.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn origin_push_keeps_the_commit_on_lost_delivery() {
        // Pushsync books at dispatch like retrieval: an explicit storer rejection
        // (`Remote`) is indistinguishable from a post-charge reset, so the
        // dispatch commit stays.
        let (handle, accounting, _settlement, mut rx) = gated_handle(100);
        let peer = peer(7);
        let stamped = test_stamped_chunk();

        let task = tokio::spawn({
            let handle = handle.clone();
            async move { handle.push_chunk(peer, stamped, true).await }
        });
        let response = match rx.recv().await.expect("dispatched") {
            ClientCommand::PushChunk { response, .. } => response,
            other => panic!("unexpected command: {other:?}"),
        };
        assert_eq!(Ledger::balance(&*accounting, &peer), Au::new(-100));

        response.send(Err(ChunkTransferError::Remote)).ok();
        let _ = task.await;
        assert_eq!(Ledger::reserved(&*accounting, &peer), Au::ZERO);
        assert_eq!(Ledger::balance(&*accounting, &peer), Au::new(-100));
    }

    #[tokio::test]
    async fn origin_push_refunds_when_peer_unreached() {
        // A push that never reached a peer (`NotConnected`) charged nothing, so
        // the dispatch commit is refunded.
        let (handle, accounting, _settlement, mut rx) = gated_handle(100);
        let peer = peer(11);
        let stamped = test_stamped_chunk();

        let task = tokio::spawn({
            let handle = handle.clone();
            async move { handle.push_chunk(peer, stamped, true).await }
        });
        let response = match rx.recv().await.expect("dispatched") {
            ClientCommand::PushChunk { response, .. } => response,
            other => panic!("unexpected command: {other:?}"),
        };
        assert_eq!(Ledger::balance(&*accounting, &peer), Au::new(-100));

        response.send(Err(ChunkTransferError::NotConnected)).ok();
        let _ = task.await;
        assert_eq!(Ledger::reserved(&*accounting, &peer), Au::ZERO);
        assert_eq!(Ledger::balance(&*accounting, &peer), Au::ZERO);
    }

    #[tokio::test]
    async fn origin_dispatch_refunds_on_send_failure() {
        // The command never reaches the wire (the channel is closed), so nothing
        // was charged: the dispatch commit is refunded and the balance returns to
        // its pre-dispatch value.
        let (handle, accounting, _settlement, rx) = gated_handle(100);
        let peer = peer(12);

        // Close the command channel so `send_command` returns `ChannelClosed`.
        drop(rx);

        let outcome = handle
            .retrieve_chunk(peer, ChunkAddress::zero(), true)
            .await;
        assert!(matches!(outcome, Err(ChunkTransferError::ChannelClosed)));
        assert_eq!(Ledger::balance(&*accounting, &peer), Au::ZERO);
        assert_eq!(Ledger::reserved(&*accounting, &peer), Au::ZERO);
    }

    #[tokio::test]
    async fn dropped_race_loser_keeps_the_dispatch_commit() {
        // The key guarantee over commit-on-failure: a losing race leg's future is
        // dropped mid-await (its result never arrives), yet the debit stays
        // committed because the commit happened synchronously at dispatch. Under
        // the old reserve-only design the hold would drop un-applied and release,
        // letting our view fall below the server's. Here the balance stays
        // debited.
        let (handle, accounting, _settlement, mut rx) = gated_handle(100);
        let peer = peer(13);

        let task = tokio::spawn({
            let handle = handle.clone();
            async move {
                handle
                    .retrieve_chunk(peer, ChunkAddress::zero(), true)
                    .await
            }
        });
        // Drain the command so the dispatch (and its commit) has happened, then
        // hold the response so the future is parked awaiting it.
        let _response = match rx.recv().await.expect("dispatched") {
            ClientCommand::RetrieveChunk { response, .. } => response,
            other => panic!("unexpected command: {other:?}"),
        };
        assert_eq!(Ledger::balance(&*accounting, &peer), Au::new(-100));

        // Drop the in-flight future mid-await (the race loser).
        task.abort();
        let _ = task.await;

        // The commit is retained: dropping the future cannot un-book it.
        assert_eq!(Ledger::balance(&*accounting, &peer), Au::new(-100));
        assert_eq!(Ledger::reserved(&*accounting, &peer), Au::ZERO);
    }

    #[tokio::test]
    async fn commit_and_refund_are_ledger_only() {
        // The origin gate carries no peer reporter, so neither the dispatch commit
        // nor a refund can feed scoring; the only observable side-channel is the
        // settle trigger, which an in-band admit never fires. One delivery (commit
        // kept) and one confirmed absence (refund) leave the settle recorder
        // empty.
        let (handle, accounting, settlement, mut rx) = gated_handle(100);

        // A delivered request keeps its commit, with no settle.
        let kept = peer(14);
        let task = tokio::spawn({
            let handle = handle.clone();
            async move {
                handle
                    .retrieve_chunk(kept, ChunkAddress::zero(), true)
                    .await
            }
        });
        let response = match rx.recv().await.expect("dispatched") {
            ClientCommand::RetrieveChunk { response, .. } => response,
            other => panic!("unexpected command: {other:?}"),
        };
        response
            .send(Ok(RetrievalResult {
                chunk: content_chunk(),
                stamp: None,
                peer: kept,
            }))
            .expect("receiver alive");
        task.await.unwrap().expect("delivery ok");
        assert_eq!(Ledger::balance(&*accounting, &kept), Au::new(-100));

        // A confirmed absence refunds, also with no settle.
        let refunded = peer(15);
        let task = tokio::spawn({
            let handle = handle.clone();
            async move {
                handle
                    .retrieve_chunk(refunded, ChunkAddress::zero(), true)
                    .await
            }
        });
        let response = match rx.recv().await.expect("dispatched") {
            ClientCommand::RetrieveChunk { response, .. } => response,
            other => panic!("unexpected command: {other:?}"),
        };
        response
            .send(Err(ChunkTransferError::NotFound(ChunkAddress::zero())))
            .expect("receiver alive");
        let _ = task.await;
        assert_eq!(Ledger::balance(&*accounting, &refunded), Au::ZERO);

        assert!(
            settlement.triggered.lock().unwrap().is_empty(),
            "in-band commit and refund trigger no settle and no scoring"
        );
    }

    #[tokio::test]
    async fn origin_dispatch_without_a_gate_is_a_noop() {
        // The lightweight launcher attaches no origin gate; an origin dispatch
        // must still send and never panic.
        let (tx, mut rx) = mpsc::channel::<ClientCommand>(16);
        let handle = ClientHandle::new(tx);
        let task = tokio::spawn({
            let handle = handle.clone();
            async move {
                handle
                    .retrieve_chunk(peer(8), ChunkAddress::zero(), true)
                    .await
            }
        });
        match rx.recv().await.expect("dispatched without a gate") {
            ClientCommand::RetrieveChunk { response, .. } => {
                response.send(Err(ChunkTransferError::Remote)).ok();
            }
            other => panic!("unexpected command: {other:?}"),
        }
        let _ = task.await;
    }

    use vertex_swarm_accounting::DefaultBandwidthConfig;
    use vertex_swarm_api::Ledger;
    use vertex_swarm_test_utils::MockIdentity;

    #[tokio::test]
    async fn dispatch_sends_immediately_for_an_admissible_peer() {
        // An in-band peer dispatches without waiting: the command is emitted
        // promptly, with no pacing delay between the call and the send.
        let (handle, _accounting, _settlement, mut rx) = gated_handle(100);
        let peer = peer(1);
        let stamped = test_stamped_chunk();
        let push = tokio::spawn(async move { handle.push_chunk(peer, stamped, true).await });

        let cmd = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("admissible push dispatched promptly")
            .expect("command emitted");
        match cmd {
            ClientCommand::PushChunk { response, .. } => {
                response.send(Err(ChunkTransferError::Remote)).ok();
            }
            other => panic!("unexpected command: {other:?}"),
        }
        let _ = push.await;
    }

    fn test_stamped_chunk() -> StampedChunk {
        use nectar_primitives::ContentChunk;
        let chunk = ContentChunk::new(&b"dispatch-test"[..]).expect("valid content chunk");
        StampedChunk::new(chunk.into(), test_stamp())
    }

    fn test_stamp() -> vertex_swarm_api::Stamp {
        use alloy_primitives::{B256, Signature};
        use vertex_swarm_api::Stamp;
        let mut raw = [0u8; 65];
        raw[..64].fill(1);
        raw[64] = 27;
        let sig = Signature::try_from(&raw[..]).expect("valid signature bytes");
        Stamp::new(B256::repeat_byte(0xaa), 3, 7, 42, sig)
    }

    #[test]
    fn no_reporter_is_a_noop() {
        let (service, _event_tx, _handle) = ClientService::new();
        // Must not panic without a reporter configured.
        service.process_event(ClientEvent::RetrievalFailed {
            peer: peer(7),
            address: ChunkAddress::zero(),
            error: "x".into(),
            kind: FailureKind::InvalidChunk,
        });
    }
}
