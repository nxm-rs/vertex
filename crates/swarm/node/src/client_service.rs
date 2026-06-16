//! Client service for managing network interactions.
//!
//! The `ClientService` bridges the business logic layer with the network layer.
//! It owns channels to communicate with `ClientBehaviour` and processes
//! incoming events.

use std::sync::Arc;

use nectar_primitives::{AnyChunk, ChunkAddress};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};
use vertex_swarm_api::{PeerReporter, ReportSource, SwarmLocalStore, SwarmScoringEvent};
use vertex_swarm_net_pseudosettle::PaymentAck;
use vertex_swarm_net_pushsync::Receipt;
use vertex_swarm_primitives::{CachedChunk, OverlayAddress, Stamp, StampedChunk};
use vertex_tasks::{GracefulShutdown, SpawnableTask};

use crate::protocol::{ClientCommand, ClientEvent, FailureKind};
use crate::throttle::{ProtocolKind, SelfThrottle};

/// Report source label for retrieval-protocol peer scoring.
const RETRIEVAL_SOURCE: ReportSource = ReportSource::Protocol("retrieval");
/// Report source label for pushsync-protocol peer scoring.
const PUSHSYNC_SOURCE: ReportSource = ReportSource::Protocol("pushsync");

pub(crate) const DEFAULT_CHANNEL_CAPACITY: usize = 256;

/// Handle for sending commands to the network layer.
///
/// Request methods ([`Self::retrieve_chunk`], [`Self::push_chunk`]) thread a
/// response channel through the command into the per-connection handler, so
/// each outbound request is a self-contained future: the substream that
/// carries the request is the correlation, and no shared rendezvous state
/// exists. Concurrent requests for the same chunk address never collide, so
/// callers may freely race the same address across peers.
/// Outbound self-throttle shared by both chunk-transfer protocols.
///
/// When set, [`ClientHandle::retrieve_chunk`] and [`ClientHandle::push_chunk`]
/// pace themselves under the remote peer's pseudosettle allowance before the
/// request is dispatched. Without it, requests dispatch immediately, exactly as
/// before the throttle existed (and as the unit tests rely on).
#[derive(Clone)]
pub struct ClientHandle {
    command_tx: mpsc::Sender<ClientCommand>,
    throttle: Option<Arc<SelfThrottle>>,
}

/// Result of a chunk retrieval.
///
/// The chunk is address-validated at decode (BMT hash for content, owner plus
/// signature for single-owner), so it answers the request regardless of the
/// stamp. The stamp is optional: a storer answers a retrieval with the chunk
/// bytes and may omit the stamp from the delivery, which is never re-read on
/// this path. A stampless chunk is served to the caller; a stampless content
/// chunk is also cached by address (content is immutable), while a retrieved
/// single-owner chunk is never cached (it has no version signal).
#[derive(Debug)]
pub struct RetrievalResult {
    /// The retrieved chunk.
    pub chunk: AnyChunk,
    /// The postage stamp the responder attached, if any.
    pub stamp: Option<Stamp>,
    /// The peer that served the chunk.
    pub peer: OverlayAddress,
}

/// Outcome error shared by both chunk transfer operations.
///
/// Both [`ClientHandle::retrieve_chunk`] (get) and
/// [`ClientHandle::push_chunk`] (put) resolve through this single type. Most
/// variants are operation-agnostic and can surface from either path; the two
/// operation-specific variants are called out below.
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum ChunkTransferError {
    // Shared variants: either a get or a put can produce these.
    /// Channel closed.
    #[error("Network channel closed")]
    ChannelClosed,
    /// The target peer has no active connection.
    #[error("Peer not connected")]
    NotConnected,
    /// Request cancelled.
    #[error("Request cancelled")]
    Cancelled,
    /// The request was dispatched but the peer did not complete it within the
    /// per-protocol deadline (`retrieval_timeout` for a get, `pushsync_timeout`
    /// for a put). This is the liveness boundary against a withholding peer: the
    /// outbound substream's upgrade timeout fires and the attempt resolves here
    /// rather than hanging. It is retryable (see [`Self::is_retryable`]): another
    /// candidate may answer promptly.
    #[error("Request timed out")]
    TimedOut,
    /// Local protocol failure (dial, stream, or an inactive handler). Failures
    /// reported by the remote peer are carried by [`Self::Remote`] instead.
    #[error("Protocol error: {0}")]
    Protocol(String),
    /// The remote peer reported a failure (a retrieval error delivery or a
    /// non-success pushsync receipt). The reason is intentionally not carried:
    /// the remote's error string is adversarial input we never read.
    #[error("Remote peer reported a failure")]
    Remote,

    // Retrieval-specific: only a get produces this.
    /// Chunk not found.
    #[error("Chunk not found: {0}")]
    NotFound(ChunkAddress),
}

impl ChunkTransferError {
    /// Whether retrying the request against another candidate may succeed.
    ///
    /// A timeout, a remote-reported failure, or a transient local protocol
    /// error are all worth retrying on a fresh peer: a different candidate may
    /// hold the chunk and answer within the deadline. A cancelled or
    /// channel-closed request reflects a local teardown that another attempt
    /// cannot fix, and a not-found is the chunk's own absence at the queried
    /// peer (still potentially elsewhere, so the get path races other
    /// candidates regardless). The classification exists so callers route a
    /// withholding peer's `TimedOut` to the next candidate rather than treating
    /// it as terminal.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::TimedOut | Self::Remote | Self::Protocol(_) | Self::NotFound(_) => true,
            Self::ChannelClosed | Self::NotConnected | Self::Cancelled => false,
        }
    }
}

impl ClientHandle {
    /// Create a new client handle without outbound self-throttling.
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

    /// Send a command to the network layer (non-blocking).
    ///
    /// Uses `try_send` because callers (e.g. the libp2p event loop) must not block.
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
    /// Sends a retrieval command carrying the response channel and waits for
    /// the outcome. The request is self-contained: any failure on the path
    /// (peer not connected, queue overflow, substream error, disconnect)
    /// resolves or drops the channel, so this future never hangs.
    pub async fn retrieve_chunk(
        &self,
        peer: OverlayAddress,
        address: ChunkAddress,
    ) -> Result<RetrievalResult, ChunkTransferError> {
        // Pace ourselves under the peer's pseudosettle allowance before issuing
        // the request, so a burst does not trip the remote's refuse threshold.
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
        })?;

        rx.await.map_err(|_| ChunkTransferError::Cancelled)?
    }

    /// Push a stamped chunk to a specific peer.
    ///
    /// Sends a push command carrying the response channel and waits for the
    /// storer's [`Receipt`]. Same failure semantics as [`Self::retrieve_chunk`]:
    /// the future never hangs. The receipt is already storer-verified (the decode
    /// boundary rejects an unrecoverable receipt), so an `Ok` here always carries
    /// a recovered storer.
    pub async fn push_chunk(
        &self,
        peer: OverlayAddress,
        chunk: StampedChunk,
    ) -> Result<Receipt, ChunkTransferError> {
        let address = *chunk.address();

        // Pace ourselves under the peer's pseudosettle allowance before issuing
        // the push, so a burst does not trip the remote's refuse threshold.
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
        })?;

        rx.await.map_err(|_| ChunkTransferError::Cancelled)?
    }
}

/// Client service that processes network events.
///
/// This is the business logic layer that handles `ClientEvent` from the network.
pub struct ClientService {
    /// Handle for sending commands.
    handle: ClientHandle,
    /// Event receiver from the network.
    event_rx: mpsc::Receiver<ClientEvent>,
    /// Optional peer scoring authority. Retrieval and pushsync outcomes feed
    /// it so honest peers climb and misbehaving peers are scored down.
    /// Best-effort: without a reporter, outcomes only surface as logs.
    reporter: Option<Arc<dyn PeerReporter>>,
    /// Optional client cache. The service caches the client's own successful
    /// retrieval of a content chunk here (immutable, served indefinitely, with or
    /// without a stamp), so a later request can serve it from the cache. A
    /// retrieved single-owner chunk is never cached: it has no version signal and
    /// could serve a stale revision.
    store: Option<Arc<dyn SwarmLocalStore>>,
    /// Optional outbound self-throttle, shared with the client handle. On
    /// disconnect the service clears the peer's bucket so memory does not grow
    /// with the count of distinct peers seen and a reconnect starts fresh.
    throttle: Option<Arc<SelfThrottle>>,
}

impl ClientService {
    /// Create a new client service with default channel capacity.
    ///
    /// Returns the service and a sender for events (to be used by the network layer).
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
        };

        (service, event_tx, handle)
    }

    /// Create with explicit channels.
    ///
    /// Use this when the network layer creates the command channel.
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
        };

        (service, handle)
    }

    /// Attach a peer reporter so retrieval and pushsync outcomes feed scoring.
    ///
    /// Reporting is best-effort and non-blocking. Without a reporter, outcomes
    /// only surface as logs, exactly as before.
    #[must_use]
    pub fn with_reporter(mut self, reporter: Arc<dyn PeerReporter>) -> Self {
        self.reporter = Some(reporter);
        self
    }

    /// Attach the client cache so the service caches its own retrieval
    /// deliveries.
    ///
    /// A content chunk resolved from one of our own outbound retrievals is cached
    /// here (immutable, served indefinitely, with or without a stamp). A
    /// single-owner chunk is never cached from retrieval, since a stampless SOC
    /// has no version signal and could serve a stale revision. Without a store,
    /// deliveries are not cached and a repeat request always hits the network.
    #[must_use]
    pub fn with_store(mut self, store: Arc<dyn SwarmLocalStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// Attach the outbound self-throttle so the service clears a peer's bucket
    /// on disconnect.
    ///
    /// This must be the same [`SelfThrottle`] instance attached to the
    /// [`ClientHandle`] via [`ClientHandle::with_throttle`], so the bucket the
    /// outbound API paces against is the one cleared here.
    #[must_use]
    pub fn with_throttle(mut self, throttle: Arc<SelfThrottle>) -> Self {
        self.throttle = Some(throttle);
        self
    }

    /// Get a handle for sending commands.
    pub fn handle(&self) -> ClientHandle {
        self.handle.clone()
    }

    /// Report a scoring event for a peer if a reporter is configured.
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
            } => {
                // The requester is resolved directly by the handler; this
                // event exists for accounting, peer scoring, and caching. The
                // chunk was already verified against the requested address at
                // decode, so an honest delivery raises the peer's score. A
                // retrieved content chunk (CAC) is immutable, so it is cached by
                // address (with or without a stamp, exactly as delivered) and a
                // later request serves it locally. A retrieved SOC is never
                // cached: it arrives without a version signal and could serve a
                // stale revision, so it is delivered to the caller only.
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

            ClientEvent::InboundPushFailed { peer, address } => {
                debug!(%peer, %address, "Inbound pushsync failed (substream reset)");
                metrics::counter!("swarm.client.inbound_push_failed").increment(1);
            }

            ClientEvent::ReceiptReceived {
                peer,
                address,
                latency,
            } => {
                // The pusher is resolved directly by the handler; this event
                // exists for accounting and peer scoring.
                debug!(%peer, %address, ?latency, "Receipt received");
                self.report(
                    &peer,
                    SwarmScoringEvent::PushSuccess { latency },
                    PUSHSYNC_SOURCE,
                );
            }

            ClientEvent::PeerDisconnected { peer_id, overlay } => {
                debug!(%peer_id, %overlay, "Peer disconnected");
                // Drop the peer's throttle bucket so memory does not grow with
                // the count of distinct peers seen and a reconnect starts from a
                // fresh allowance rather than stale credit.
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
                // The requester is resolved directly by the handler; this
                // event exists for peer scoring.
                //
                // A malformed chunk is invalid data (weight -10): the peer
                // handed back bytes that failed address/stamp reconstruction,
                // which is genuine misbehaviour and is scored adversely.
                //
                // A plain `Protocol` failure (the remote reported "I don't have
                // it" / could not forward, a timeout, or a transport error) is
                // NOT scored: a peer not holding or not forwarding a requested
                // chunk is the expected, blameless outcome for the vast majority
                // of peers on any given retrieval, and on a bulk download the
                // flood of such misses would otherwise decay scores past the
                // disconnect threshold and prune the peer set (erode Kademlia
                // depth). The staggered race already steers around an unhelpful
                // candidate within a request; bee likewise uses a temporary
                // per-chunk skiplist here, never a connection-killing score
                // penalty. Only misbehaviour touches the persistent score.
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
                        // Blameless miss/timeout: count it for visibility but do
                        // not penalise the peer's score.
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
                // The pusher is resolved directly by the handler; this event
                // exists for peer scoring. Same classification as retrieval: a
                // malformed receipt is invalid data (scored), while a plain
                // `Protocol` failure (the peer could not store/forward, a
                // timeout, or a transport error) is a blameless outcome that
                // must not drive the peer toward disconnection.
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
                // A peer pushed us a malformed chunk or sent a malformed
                // retrieval request: the decode rejected it and it was never
                // relayed. Score the sender adversely for invalid data.
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

            ClientEvent::SettlementNeeded { peer, balance } => {
                debug!(%peer, %balance, "Settlement needed");
                // TODO: Initiate settlement (swap or pseudosettle)
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

                // Send ack with current timestamp
                let ack = PaymentAck::now(amount);

                if let Err(e) = self.handle.send_command(ClientCommand::AckPseudosettle {
                    peer,
                    request_id,
                    ack,
                }) {
                    warn!(%peer, %peer_id, error = ?e, "Failed to send pseudosettle ack");
                }
            }

            ClientEvent::PseudosettleSent { peer, peer_id, ack } => {
                debug!(%peer, %peer_id, amount = %ack.amount, timestamp = ack.timestamp, "Pseudosettle sent, received ack");
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
        }
    }
}

impl Default for ClientService {
    fn default() -> Self {
        Self::new().0
    }
}

impl SpawnableTask for ClientService {
    fn into_task(self, shutdown: GracefulShutdown) -> impl std::future::Future<Output = ()> + Send {
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
        /// Return the single recorded report, asserting exactly one exists.
        fn single(&self) -> (OverlayAddress, SwarmScoringEvent, ReportSource) {
            let reports = self.reports.lock().unwrap();
            assert_eq!(reports.len(), 1, "expected exactly one report");
            *reports.first().expect("one report")
        }

        /// Assert that no scoring report was recorded.
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
        // A blameless miss/timeout (`FailureKind::Protocol`) is the expected
        // outcome for a peer that simply does not hold or cannot forward the
        // chunk. It must not touch the peer's persistent score, so a bulk
        // download cannot decay the peer set past the disconnect threshold.
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
        // Mirror of the retrieval case: a peer that could not store/forward a
        // pushed chunk (timeout, transport error, remote-reported failure) is
        // not misbehaving and must not be scored toward disconnection.
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

    // Throttle wiring at the outbound-API boundary.
    use crate::throttle::SelfThrottle;
    use vertex_swarm_api::{
        Au, BandwidthMode, PeerAffordability, SwarmBandwidthAccounting, SwarmClientAccounting,
        SwarmPricing, SwarmResult,
    };
    use vertex_swarm_bandwidth::{
        DefaultBandwidthConfig, NoAccounting, NoPeerBandwidth, NoProvideAction, NoReceiveAction,
    };
    use vertex_swarm_test_utils::MockIdentity;

    /// A fixed per-peer allowance, in AU, for the throttle's allowance signal.
    ///
    /// Also stands in as the [`SwarmBandwidthAccounting`] half of the client
    /// accounting mock: the throttle reads only affordability and pricing off the
    /// accounting object, so the accounting surface is a no-op.
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

    /// A pricer that meters every chunk at one AU, so the throttle's bucket holds
    /// exactly `tokens` requests.
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

    /// Minimal [`SwarmClientAccounting`] bundling a fixed allowance and the
    /// one-AU pricer so [`SelfThrottle::new`] can extract both.
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

    /// Build a handle whose throttle gives each peer a bucket of `tokens`
    /// one-AU requests (refresh rate and per-request chunk price are both 1 AU,
    /// so the bucket holds exactly `tokens` requests and refills one per second).
    fn throttled_handle(tokens: u64) -> (ClientHandle, mpsc::Receiver<ClientCommand>) {
        let (tx, rx) = mpsc::channel::<ClientCommand>(16);
        let accounting = MockClientAccounting {
            bandwidth: FixedAllowance(tokens),
            pricing: OneAuPricer,
        };
        // Only refresh_rate (1 AU/sec) and throttle_allowance_percent (100) are
        // read off the config; the rest are placeholders the throttle ignores.
        let config = DefaultBandwidthConfig::new(
            BandwidthMode::Pseudosettle,
            0,
            0,
            1,
            0,
            1,
            100,
            Default::default(),
        );
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
        let push = tokio::spawn(async move { handle.push_chunk(peer, stamped).await });

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
            let (_outcome, ()) = tokio::join!(handle.retrieve_chunk(peer(1), address), serve);
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
        // Without a throttle the handle behaves exactly as before: no pacing.
        let (tx, mut rx) = mpsc::channel::<ClientCommand>(16);
        let handle = ClientHandle::new(tx);
        let address = test_address();
        let task = tokio::spawn({
            let handle = handle.clone();
            async move { handle.retrieve_chunk(peer(1), address).await }
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
