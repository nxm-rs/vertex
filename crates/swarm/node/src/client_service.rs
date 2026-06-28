//! Client service bridging business logic and the network layer.
//!
//! Owns channels to `ClientBehaviour` and processes incoming events.

use std::sync::Arc;

use nectar_primitives::ChunkAddress;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};
use vertex_swarm_api::{
    Admission, AdmissionControl, Au, BandwidthReserve, HeldReceive, PeerReporter, ReportSource,
    SwarmLocalStore, SwarmPricing, SwarmScoringEvent,
};
use vertex_swarm_client_protocol::PseudosettleAck;
pub use vertex_swarm_client_protocol::{ChunkTransferError, RetrievalResult};
use vertex_swarm_net_pushsync::Receipt;
use vertex_swarm_primitives::{CachedChunk, OverlayAddress, StampedChunk};
use vertex_tasks::{GracefulShutdown, MaybeSend, SpawnableTask};

use crate::inflight::PeerInflightLimiter;
use crate::protocol::{ClientCommand, ClientEvent, FailureKind};
use crate::selection::SettlementTrigger;
use crate::throttle::{ProtocolKind, SelfThrottle};

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
    /// When set, requests pace themselves under the peer's pseudosettle
    /// allowance before dispatch.
    throttle: Option<Arc<SelfThrottle>>,
    /// When set, an origin request reserves its price at dispatch, gates on the
    /// admission band, and commits the reserved debit on delivery. Absent on the
    /// lightweight launcher, where origin dispatch neither reserves nor gates.
    origin: Option<OriginGate>,
}

/// Reserve-at-dispatch and the admission band for origin requests.
///
/// All four read the one shared accounting the selector and throttle use, so the
/// dispatch gate, the candidate selector, and the in-flight reservation agree on
/// one ledger. The settlement trigger is the selector's, so settles dedup across
/// both paths.
#[derive(Clone)]
struct OriginGate {
    pricing: Arc<dyn SwarmPricing>,
    reserve: Arc<dyn BandwidthReserve>,
    admission: Arc<dyn AdmissionControl>,
    settlement: Arc<dyn SettlementTrigger>,
}

impl ClientHandle {
    /// Create a handle without outbound self-throttling.
    pub fn new(command_tx: mpsc::Sender<ClientCommand>) -> Self {
        Self {
            command_tx,
            throttle: None,
            origin: None,
        }
    }

    /// Attach the outbound self-throttle so retrieval and pushsync pace
    /// themselves under each peer's pseudosettle allowance.
    #[must_use]
    pub fn with_throttle(mut self, throttle: Arc<SelfThrottle>) -> Self {
        self.throttle = Some(throttle);
        self
    }

    /// Attach the origin credit gate so an own-request dispatch reserves its
    /// price, bands the request, and commits the debit on delivery.
    ///
    /// `pricing`, `reserve`, and `admission` must read the one shared accounting
    /// the selector and throttle use, and `settlement` must be the selector's so
    /// the in-flight settle dedup is shared. Relay legs (`originated == false`)
    /// are accounted by the forwarder and bypass this gate.
    #[must_use]
    pub fn with_origin_gate(
        mut self,
        pricing: Arc<dyn SwarmPricing>,
        reserve: Arc<dyn BandwidthReserve>,
        admission: Arc<dyn AdmissionControl>,
        settlement: Arc<dyn SettlementTrigger>,
    ) -> Self {
        self.origin = Some(OriginGate {
            pricing,
            reserve,
            admission,
            settlement,
        });
        self
    }

    /// Gate and reserve an origin request before dispatch.
    ///
    /// Returns the hold to carry across the in-flight leg (`Ok(Some(_))`), or
    /// `Ok(None)` for a relay leg or when no origin gate is attached. An
    /// [`Admit`](Admission::Admit) band reserves and sends; a
    /// [`SettleAndAdmit`](Admission::SettleAndAdmit) triggers a settle and sends
    /// anyway (the band keeps us under the disconnect line whichever order the
    /// storer applies them); a [`Refuse`](Admission::Refuse) at the disconnect
    /// line triggers a settle and refuses, so the caller routes elsewhere.
    fn reserve_origin(
        &self,
        peer: OverlayAddress,
        address: &ChunkAddress,
        originated: bool,
    ) -> Result<Option<Box<dyn HeldReceive>>, ChunkTransferError> {
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

        // The reserve shares the admit boundary, so it refuses at the disconnect
        // line too; a concurrent burst that crossed it between the band check and
        // here surfaces as a refusal, handled identically.
        match gate.reserve.reserve_received(peer, price, true) {
            Ok(held) => Ok(Some(held)),
            Err(_) => {
                gate.settlement.trigger_settlement(peer);
                Err(ChunkTransferError::Refused)
            }
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
        if let Some(throttle) = &self.throttle {
            throttle
                .acquire(peer, address, ProtocolKind::Retrieval)
                .await;
        }

        // Reserve the origin debit and gate on the band before sending. The hold
        // rides this future: applied on delivery, released on any other exit
        // (failure, cancel, a dropped losing race leg).
        let reservation = self.reserve_origin(peer, &address, originated)?;

        let (tx, rx) = oneshot::channel();

        self.send_command(ClientCommand::RetrieveChunk {
            peer,
            address,
            response: tx,
            originated,
        })?;

        let result = rx.await.map_err(|_| ChunkTransferError::Cancelled)?;
        if result.is_ok()
            && let Some(held) = reservation
        {
            held.apply();
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

        if let Some(throttle) = &self.throttle {
            throttle
                .acquire(peer, address, ProtocolKind::Pushsync)
                .await;
        }

        // Pushsync reserves the origin debit and gates like retrieval. A
        // `SettleAndAdmit` peer is still sent to (closeness preserved); only a
        // `Refuse` at the disconnect line refuses here.
        let reservation = self.reserve_origin(peer, &address, originated)?;

        let (tx, rx) = oneshot::channel();

        self.send_command(ClientCommand::PushChunk {
            peer,
            address,
            chunk,
            response: tx,
            originated,
        })?;

        let result = rx.await.map_err(|_| ChunkTransferError::Cancelled)?;
        if result.is_ok()
            && let Some(held) = reservation
        {
            held.apply();
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
    /// Outbound self-throttle shared with the handle; cleared per peer on
    /// disconnect so memory does not grow with distinct peers seen.
    throttle: Option<Arc<SelfThrottle>>,
    /// Per-peer retrieval in-flight limiter shared with the chunk provider;
    /// the peer entry is forgotten on disconnect.
    inflight: Option<Arc<PeerInflightLimiter>>,
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
            throttle: None,
            inflight: None,
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
            throttle: None,
            inflight: None,
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

    /// Attach the outbound self-throttle so the service clears a peer's bucket on
    /// disconnect.
    ///
    /// Must be the same [`SelfThrottle`] instance attached to the
    /// [`ClientHandle`] via [`ClientHandle::with_throttle`].
    #[must_use]
    pub fn with_throttle(mut self, throttle: Arc<SelfThrottle>) -> Self {
        self.throttle = Some(throttle);
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
                originated: _,
            } => {
                // The requester is resolved by the handler; this event exists for
                // scoring and caching. Content chunks are cached by address
                // (immutable); SOCs are not (no version signal). The origin debit
                // is committed by the dispatch reservation, not here; a relay leg
                // is accounted by the forwarder. Cache and scoring apply to every
                // delivery.
                debug!(%peer, %address, ?latency, "Chunk received");
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
                if let Some(throttle) = &self.throttle {
                    throttle.clear(&overlay);
                }
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
            DefaultBandwidthConfig::new(1000, 25, 10, 60, 1, 50, FixedPricingConfig::default());
        let accounting = Arc::new(Accounting::new(config, MockIdentity::with_first_byte(0)));
        let settlement = Arc::new(RecordingSettlement::default());
        let (tx, rx) = mpsc::channel::<ClientCommand>(16);
        let handle = ClientHandle::new(tx).with_origin_gate(
            Arc::new(FixedPeerPricer(price)) as Arc<dyn SwarmPricing>,
            accounting.clone() as Arc<dyn BandwidthReserve>,
            accounting.clone() as Arc<dyn AdmissionControl>,
            settlement.clone() as Arc<dyn SettlementTrigger>,
        );
        (handle, accounting, settlement, rx)
    }

    #[tokio::test]
    async fn origin_dispatch_reserves_then_applies_on_delivery() {
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

        // The dispatched command means the reservation is held: the price is
        // reserved and the committed balance is still zero.
        let response = match rx.recv().await.expect("dispatched") {
            ClientCommand::RetrieveChunk {
                peer: p, response, ..
            } => {
                assert_eq!(p, peer);
                response
            }
            other => panic!("unexpected command: {other:?}"),
        };
        assert_eq!(Ledger::reserved(&*accounting, &peer), Au::from_amount(100));
        assert_eq!(Ledger::balance(&*accounting, &peer), Au::ZERO);

        // Delivery applies the reservation: debited once, reserve cleared.
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
    async fn origin_dispatch_releases_reservation_on_failure() {
        let (handle, accounting, _settlement, mut rx) = gated_handle(100);
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
        assert_eq!(Ledger::reserved(&*accounting, &peer), Au::from_amount(100));

        // A remote failure releases the reservation, leaving the balance untouched.
        response
            .send(Err(ChunkTransferError::Remote))
            .expect("receiver alive");
        assert!(matches!(
            task.await.unwrap(),
            Err(ChunkTransferError::Remote)
        ));
        assert_eq!(Ledger::balance(&*accounting, &peer), Au::ZERO);
        assert_eq!(Ledger::reserved(&*accounting, &peer), Au::ZERO);
    }

    #[tokio::test]
    async fn origin_dispatch_releases_reservation_on_dropped_response() {
        // The cancel path: the response oneshot is dropped without an answer, so
        // the request returns `Cancelled` and the held reservation releases. No
        // leak, no debit.
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
        assert_eq!(Ledger::reserved(&*accounting, &peer), Au::from_amount(100));

        drop(response);
        assert!(matches!(
            task.await.unwrap(),
            Err(ChunkTransferError::Cancelled)
        ));
        assert_eq!(Ledger::reserved(&*accounting, &peer), Au::ZERO);
        assert_eq!(Ledger::balance(&*accounting, &peer), Au::ZERO);
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
    async fn origin_push_reserves_then_releases_on_failure() {
        // Pushsync reserves at dispatch like retrieval and releases on failure.
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
        assert_eq!(Ledger::reserved(&*accounting, &peer), Au::from_amount(100));

        response.send(Err(ChunkTransferError::Remote)).ok();
        let _ = task.await;
        assert_eq!(Ledger::reserved(&*accounting, &peer), Au::ZERO);
        assert_eq!(Ledger::balance(&*accounting, &peer), Au::ZERO);
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

    // Throttle wiring at the outbound API boundary.
    use crate::throttle::SelfThrottle;
    use vertex_swarm_accounting::{
        DefaultBandwidthConfig, NoAccounting, NoPeerBandwidth, NoProvideAction, NoReceiveAction,
    };
    use vertex_swarm_api::{
        Au, Ledger, SwarmBandwidthAccounting, SwarmClientAccounting, SwarmPricing, SwarmResult,
        Threshold,
    };
    use vertex_swarm_test_utils::MockIdentity;

    /// A fixed per-peer headroom, in AU, for the throttle's allowance signal.
    /// Also serves as a no-op [`SwarmBandwidthAccounting`] half of the mock.
    #[derive(Clone)]
    struct FixedAllowance(u64);
    impl Ledger for FixedAllowance {
        fn balance(&self, _overlay: &OverlayAddress) -> Au {
            Au::ZERO
        }
        fn reserved(&self, _overlay: &OverlayAddress) -> Au {
            Au::ZERO
        }
        fn headroom(&self, _overlay: &OverlayAddress, _to: Threshold) -> Au {
            Au::from_amount(self.0)
        }
        fn disconnect_line(&self, _overlay: &OverlayAddress) -> Au {
            Au::from_amount(self.0)
        }
        fn settle_trigger(&self, _overlay: &OverlayAddress) -> Au {
            Au::from_amount(self.0)
        }
    }

    impl SwarmBandwidthAccounting for FixedAllowance {
        type Identity = MockIdentity;
        type Peer = NoPeerBandwidth;
        type ReceiveAction = NoReceiveAction;
        type ProvideAction = NoProvideAction;

        fn identity(&self) -> &Self::Identity {
            unreachable!("throttle never reads the identity")
        }
        fn for_peer(&self, peer: OverlayAddress) -> Self::Peer {
            NoAccounting::new(MockIdentity::with_first_byte(0)).for_peer(peer)
        }
        fn peers(&self) -> Vec<OverlayAddress> {
            Vec::new()
        }
        fn remove_peer(&self, _peer: &OverlayAddress) {}
        fn prepare_receive(
            &self,
            _peer: OverlayAddress,
            _price: Au,
            _originated: bool,
        ) -> SwarmResult<Self::ReceiveAction> {
            Ok(NoReceiveAction)
        }
        fn prepare_provide(
            &self,
            _peer: OverlayAddress,
            _price: Au,
        ) -> SwarmResult<Self::ProvideAction> {
            Ok(NoProvideAction)
        }
    }

    /// Meters every chunk at one AU, so the bucket holds exactly `tokens`
    /// requests.
    #[derive(Clone)]
    struct OneAuPricer;
    impl SwarmPricing for OneAuPricer {
        fn price(&self, _chunk: &ChunkAddress) -> Au {
            Au::from_amount(1)
        }
        fn peer_price(&self, _peer: &OverlayAddress, _chunk: &ChunkAddress) -> Au {
            Au::from_amount(1)
        }
    }

    /// Bundles the fixed allowance and one-AU pricer for [`SelfThrottle::new`].
    #[derive(Clone)]
    struct MockClientAccounting {
        bandwidth: FixedAllowance,
        pricing: OneAuPricer,
    }
    impl SwarmClientAccounting for MockClientAccounting {
        type Bandwidth = FixedAllowance;
        type Pricing = OneAuPricer;

        fn bandwidth(&self) -> &Self::Bandwidth {
            &self.bandwidth
        }
        fn pricing(&self) -> &Self::Pricing {
            &self.pricing
        }
    }

    /// Build a handle whose throttle gives each peer a bucket of `tokens` one-AU
    /// requests, refilling one per second.
    fn throttled_handle(tokens: u64) -> (ClientHandle, mpsc::Receiver<ClientCommand>) {
        let (tx, rx) = mpsc::channel::<ClientCommand>(16);
        let accounting = MockClientAccounting {
            bandwidth: FixedAllowance(tokens),
            pricing: OneAuPricer,
        };
        // Only refresh_rate (1 AU/sec) and throttle_allowance_percent (100) are
        // read; the rest are placeholders.
        let config = DefaultBandwidthConfig::new(0, 0, 1, 0, 1, 100, Default::default());
        let throttle = Arc::new(SelfThrottle::new(&accounting, &config));
        (ClientHandle::new(tx).with_throttle(throttle), rx)
    }

    #[tokio::test]
    async fn throttled_push_dispatches_under_budget() {
        // A generous allowance must not delay the first push: the command is
        // dispatched promptly.
        let (handle, mut rx) = throttled_handle(100);
        let peer = peer(1);
        let stamped = test_stamped_chunk();
        let push = tokio::spawn(async move { handle.push_chunk(peer, stamped, true).await });

        let cmd = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("push dispatched under budget")
            .expect("command emitted");
        match cmd {
            ClientCommand::PushChunk { response, .. } => {
                response.send(Err(ChunkTransferError::Remote)).ok();
            }
            other => panic!("unexpected command: {other:?}"),
        }
        let _ = push.await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn throttled_retrieval_delays_when_bucket_drained() {
        // A one-token bucket admits the first retrieval, then must throttle the
        // second until the bucket refills (a one-second window). This runs on
        // the real clock because the parking timer (`futures-timer`, chosen for
        // wasm) does not honor tokio's paused test clock; a multi-thread runtime
        // lets the parked retrieval and the receiver progress in parallel.
        let (handle, mut rx) = throttled_handle(1);
        let address = test_address();

        // Drive each retrieval concurrently with one receiver step, answering the
        // command as it arrives and returning how long the call took.
        async fn one_retrieval(
            handle: &ClientHandle,
            rx: &mut mpsc::Receiver<ClientCommand>,
            address: ChunkAddress,
        ) -> Duration {
            let start = std::time::Instant::now();
            let serve = async {
                if let Some(ClientCommand::RetrieveChunk { response, .. }) = rx.recv().await {
                    response
                        .send(Err(ChunkTransferError::Protocol("done".into())))
                        .ok();
                }
            };
            let (_outcome, ()) = tokio::join!(handle.retrieve_chunk(peer(1), address, true), serve);
            start.elapsed()
        }

        // First call drains the single token immediately.
        let first = one_retrieval(&handle, &mut rx, address).await;
        assert!(
            first < Duration::from_millis(500),
            "first retrieval should not be throttled, took {first:?}"
        );

        // Second call must wait out the refill window before its command is
        // dispatched and answered.
        let second = one_retrieval(&handle, &mut rx, address).await;
        assert!(
            second >= Duration::from_millis(500),
            "second retrieval should be throttled by the drained bucket, took {second:?}"
        );
    }

    #[tokio::test]
    async fn unthrottled_handle_dispatches_immediately() {
        // Without a throttle there is no pacing.
        let (tx, mut rx) = mpsc::channel::<ClientCommand>(16);
        let handle = ClientHandle::new(tx);
        let address = test_address();
        let task = tokio::spawn({
            let handle = handle.clone();
            async move { handle.retrieve_chunk(peer(1), address, true).await }
        });
        let cmd = rx.recv().await.expect("command emitted");
        assert!(matches!(cmd, ClientCommand::RetrieveChunk { .. }));
        task.abort();
    }

    fn test_stamped_chunk() -> StampedChunk {
        use nectar_primitives::ContentChunk;
        let chunk = ContentChunk::new(&b"throttle-test"[..]).expect("valid content chunk");
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

    fn test_address() -> ChunkAddress {
        *test_stamped_chunk().address()
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
