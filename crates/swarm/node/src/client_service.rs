//! Client service bridging business logic and the network layer.
//!
//! Owns channels to `ClientBehaviour` and processes incoming events.

use std::sync::Arc;

use nectar_primitives::ChunkAddress;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};
use vertex_swarm_api::{
    Au, BandwidthDebit, PeerReporter, ReportSource, SwarmLocalStore, SwarmPricing,
    SwarmScoringEvent,
};
use vertex_swarm_client_protocol::PseudosettleAck;
pub use vertex_swarm_client_protocol::{ChunkTransferError, RetrievalResult};
use vertex_swarm_net_pushsync::Receipt;
use vertex_swarm_primitives::{CachedChunk, OverlayAddress, StampedChunk};
use vertex_tasks::{GracefulShutdown, MaybeSend, SpawnableTask};

use crate::protocol::{ClientCommand, ClientEvent, FailureKind};
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
}

impl ClientHandle {
    /// Create a handle without outbound self-throttling.
    pub fn new(command_tx: mpsc::Sender<ClientCommand>) -> Self {
        Self {
            command_tx,
            throttle: None,
        }
    }

    /// Attach the outbound self-throttle so retrieval and pushsync pace
    /// themselves under each peer's pseudosettle allowance.
    #[must_use]
    pub fn with_throttle(mut self, throttle: Arc<SelfThrottle>) -> Self {
        self.throttle = Some(throttle);
        self
    }

    /// Whether an outbound self-throttle is attached to this handle.
    #[must_use]
    pub fn has_throttle(&self) -> bool {
        self.throttle.is_some()
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

        let (tx, rx) = oneshot::channel();

        self.send_command(ClientCommand::RetrieveChunk {
            peer,
            address,
            response: tx,
            originated,
        })?;

        rx.await.map_err(|_| ChunkTransferError::Cancelled)?
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

        let (tx, rx) = oneshot::channel();

        self.send_command(ClientCommand::PushChunk {
            peer,
            address,
            chunk,
            response: tx,
            originated,
        })?;

        rx.await.map_err(|_| ChunkTransferError::Cancelled)?
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
    /// Per-peer chunk pricer and the receive-debit half of the shared
    /// accounting. Present only on the full builder; absent on the lightweight
    /// launcher, where the origin debit is a no-op.
    accounting: Option<OriginAccounting>,
}

/// The pricing and debit handles the service needs to debit own-request
/// deliveries. Both halves come from the one shared accounting instance.
struct OriginAccounting {
    pricing: Arc<dyn SwarmPricing>,
    bandwidth: Arc<dyn BandwidthDebit>,
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
            accounting: None,
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
            accounting: None,
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

    /// Attach the shared accounting so own-request deliveries debit the serving
    /// peer by the per-chunk price.
    ///
    /// `pricing` and `bandwidth` are the two halves of the one accounting
    /// instance the selector, throttle, and forwarder also share. Only origin
    /// (own-request) completions are debited here; relay legs are accounted by
    /// the forwarder, so debiting them here would double-charge.
    #[must_use]
    pub fn with_accounting(
        mut self,
        pricing: Arc<dyn SwarmPricing>,
        bandwidth: Arc<dyn BandwidthDebit>,
    ) -> Self {
        self.accounting = Some(OriginAccounting { pricing, bandwidth });
        self
    }

    /// Whether an outbound self-throttle is attached to this service.
    #[must_use]
    pub fn has_throttle(&self) -> bool {
        self.throttle.is_some()
    }

    /// Get a handle for sending commands.
    pub fn handle(&self) -> ClientHandle {
        self.handle.clone()
    }

    /// Debit the serving peer by the per-chunk price for a completed
    /// own-request delivery. A no-op when no accounting handle is attached.
    ///
    /// The chunk is already in hand, so the debit commits immediately. A
    /// disconnect-threshold breach is already reported to peer scoring inside
    /// accounting; this only logs it.
    fn debit_origin(&self, peer: &OverlayAddress, address: &ChunkAddress) {
        let Some(accounting) = &self.accounting else {
            return;
        };
        let price = accounting.pricing.peer_price(peer, address);
        if let Err(error) = accounting.bandwidth.debit_received(*peer, price, true) {
            debug!(%peer, %address, %error, "origin debit refused at disconnect threshold");
        }
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
                // accounting, scoring, and caching. Content chunks are cached by
                // address (immutable); SOCs are not (no version signal). The
                // debit is origin-gated (a relay leg is debited by the
                // forwarder); cache and scoring apply to every delivery.
                debug!(%peer, %address, ?latency, "Chunk received");
                if originated {
                    self.debit_origin(&peer, &address);
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
                originated,
            } => {
                // The pusher is resolved by the handler; this event exists for
                // accounting and scoring. The debit is origin-gated; a relay leg
                // is debited by the forwarder.
                debug!(%peer, %address, ?latency, "Receipt received");
                if originated {
                    self.debit_origin(&peer, &address);
                }
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
                    accepted: Au::from_amount(amount.as_limbs()[0]),
                    timestamp: vertex_util_runtime::time::now_unix_nanos(),
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

    /// A fixed per-chunk price for the origin-debit tests.
    const ORIGIN_PRICE: u64 = 7;

    /// Records every receive debit so a test can assert exactly which peer was
    /// charged, how much, and that the `originated` flag carried through.
    #[derive(Default)]
    struct RecordingDebit {
        debits: Mutex<Vec<(OverlayAddress, Au, bool)>>,
    }

    impl vertex_swarm_api::BandwidthDebit for RecordingDebit {
        fn debit_received(
            &self,
            peer: OverlayAddress,
            price: Au,
            originated: bool,
        ) -> vertex_swarm_api::SwarmResult<()> {
            self.debits.lock().unwrap().push((peer, price, originated));
            Ok(())
        }
    }

    /// A pricer charging `ORIGIN_PRICE` for every peer and chunk.
    struct FixedPricer;
    impl vertex_swarm_api::SwarmPricing for FixedPricer {
        fn price(&self, _chunk: &ChunkAddress) -> Au {
            Au::from_amount(ORIGIN_PRICE)
        }
        fn peer_price(&self, _peer: &OverlayAddress, _chunk: &ChunkAddress) -> Au {
            Au::from_amount(ORIGIN_PRICE)
        }
    }

    fn service_with_accounting() -> (ClientService, Arc<RecordingDebit>) {
        let debit = Arc::new(RecordingDebit::default());
        let (service, _event_tx, _handle) = ClientService::new();
        let service = service.with_accounting(
            Arc::new(FixedPricer) as Arc<dyn SwarmPricing>,
            Arc::clone(&debit) as Arc<dyn BandwidthDebit>,
        );
        (service, debit)
    }

    fn content_chunk() -> nectar_primitives::AnyChunk {
        nectar_primitives::ContentChunk::new(&b"origin-debit-test"[..])
            .expect("valid content chunk")
            .into()
    }

    #[test]
    fn origin_retrieval_debits_serving_peer_exactly_once() {
        let (service, debit) = service_with_accounting();
        service.process_event(ClientEvent::ChunkReceived {
            peer: peer(1),
            address: ChunkAddress::zero(),
            chunk: content_chunk(),
            stamp: None,
            latency: Duration::from_millis(1),
            originated: true,
        });
        let debits = debit.debits.lock().unwrap();
        assert_eq!(
            *debits,
            vec![(peer(1), Au::from_amount(ORIGIN_PRICE), true)],
            "an origin retrieval debits the serving peer once by the per-chunk price"
        );
    }

    #[test]
    fn relay_retrieval_is_not_debited_by_the_service() {
        // The forwarder accounts a relay leg itself; the service must not also
        // debit it, or the transfer is charged twice.
        let (service, debit) = service_with_accounting();
        service.process_event(ClientEvent::ChunkReceived {
            peer: peer(2),
            address: ChunkAddress::zero(),
            chunk: content_chunk(),
            stamp: None,
            latency: Duration::from_millis(1),
            originated: false,
        });
        assert!(
            debit.debits.lock().unwrap().is_empty(),
            "a relay retrieval is debited by the forwarder, never by the service"
        );
    }

    #[test]
    fn origin_push_debits_serving_peer_exactly_once() {
        let (service, debit) = service_with_accounting();
        service.process_event(ClientEvent::ReceiptReceived {
            peer: peer(3),
            address: ChunkAddress::zero(),
            latency: Duration::from_millis(1),
            originated: true,
        });
        let debits = debit.debits.lock().unwrap();
        assert_eq!(
            *debits,
            vec![(peer(3), Au::from_amount(ORIGIN_PRICE), true)],
            "an origin push debits the storer once by the per-chunk price"
        );
    }

    #[test]
    fn relay_push_is_not_debited_by_the_service() {
        let (service, debit) = service_with_accounting();
        service.process_event(ClientEvent::ReceiptReceived {
            peer: peer(4),
            address: ChunkAddress::zero(),
            latency: Duration::from_millis(1),
            originated: false,
        });
        assert!(
            debit.debits.lock().unwrap().is_empty(),
            "a relay push is debited by the forwarder, never by the service"
        );
    }

    #[test]
    fn origin_delivery_without_accounting_is_a_noop() {
        // The lightweight launcher attaches no accounting; an origin delivery
        // must not panic and simply skips the debit.
        let (service, _event_tx, _handle) = ClientService::new();
        service.process_event(ClientEvent::ChunkReceived {
            peer: peer(5),
            address: ChunkAddress::zero(),
            chunk: content_chunk(),
            stamp: None,
            latency: Duration::from_millis(1),
            originated: true,
        });
    }

    // Throttle wiring at the outbound API boundary.
    use crate::throttle::SelfThrottle;
    use vertex_swarm_accounting::{
        DefaultBandwidthConfig, NoAccounting, NoPeerBandwidth, NoProvideAction, NoReceiveAction,
    };
    use vertex_swarm_api::{
        Au, PeerAffordability, SwarmBandwidthAccounting, SwarmClientAccounting, SwarmPricing,
        SwarmResult,
    };
    use vertex_swarm_test_utils::MockIdentity;

    /// A fixed per-peer allowance, in AU, for the throttle's allowance signal.
    /// Also serves as a no-op [`SwarmBandwidthAccounting`] half of the mock.
    #[derive(Clone)]
    struct FixedAllowance(u64);
    impl PeerAffordability for FixedAllowance {
        fn can_afford(&self, _overlay: &OverlayAddress, price: Au) -> bool {
            price.as_amount() <= self.0
        }
        fn allowance_remaining(&self, _overlay: &OverlayAddress) -> Au {
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
