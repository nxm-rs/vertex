//! Client service for managing network interactions.
//!
//! The `ClientService` bridges the business logic layer with the network layer.
//! It owns channels to communicate with `ClientBehaviour` and processes
//! incoming events.

use std::sync::Arc;

use nectar_primitives::ChunkAddress;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};
use vertex_swarm_api::{PeerReporter, PushReceipt, ReportSource, SwarmScoringEvent};
use vertex_swarm_net_pseudosettle::PaymentAck;
use vertex_swarm_primitives::{OverlayAddress, StampedChunk};
use vertex_tasks::{GracefulShutdown, SpawnableTask};

use crate::protocol::{ClientCommand, ClientEvent, FailureKind};

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
#[derive(Clone)]
pub struct ClientHandle {
    command_tx: mpsc::Sender<ClientCommand>,
}

/// Result of a chunk retrieval.
#[derive(Debug)]
pub struct RetrievalResult {
    /// The retrieved chunk and its postage stamp.
    pub chunk: StampedChunk,
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

impl ClientHandle {
    /// Create a new client handle.
    pub fn new(command_tx: mpsc::Sender<ClientCommand>) -> Self {
        Self { command_tx }
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
    /// storer's [`PushReceipt`]. Same failure semantics as
    /// [`Self::retrieve_chunk`]: the future never hangs.
    pub async fn push_chunk(
        &self,
        peer: OverlayAddress,
        chunk: StampedChunk,
    ) -> Result<PushReceipt, ChunkTransferError> {
        let address = *chunk.address();
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
                chunk: _,
                latency,
            } => {
                // The requester is resolved directly by the handler; this
                // event exists for accounting and peer scoring. The chunk was
                // already verified against the requested address at decode, so
                // an honest delivery raises the peer's score.
                debug!(%peer, %address, ?latency, "Chunk received");
                self.report(
                    &peer,
                    SwarmScoringEvent::RetrievalSuccess { latency },
                    RETRIEVAL_SOURCE,
                );
            }

            ClientEvent::ChunkRequested {
                peer,
                peer_id,
                address,
                request_id,
            } => {
                debug!(%peer_id, %peer, %address, %request_id, "Chunk requested by peer");
                // TODO: Look up chunk in local storage and serve it
            }

            ClientEvent::ChunkPushReceived {
                peer,
                peer_id,
                address,
                chunk,
                request_id,
            } => {
                debug!(
                    %peer_id, %peer, %address, %request_id,
                    stamp_batch = %chunk.stamp().batch(),
                    "Chunk push received"
                );
                // TODO: Validate chunk, store if responsible, send receipt
            }

            ClientEvent::ReceiptReceived {
                peer,
                address,
                signature,
                nonce,
                storage_radius,
                latency,
            } => {
                // The pusher is resolved directly by the handler; this event
                // exists for accounting and peer scoring.
                debug!(
                    %peer, %address, %storage_radius, %nonce, ?latency,
                    sig = %signature, "Receipt received"
                );
                self.report(
                    &peer,
                    SwarmScoringEvent::PushSuccess { latency },
                    PUSHSYNC_SOURCE,
                );
            }

            ClientEvent::PeerDisconnected { peer_id, overlay } => {
                debug!(%peer_id, %overlay, "Peer disconnected");
                // TODO: Clean up any pending operations for this peer
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
                // event exists for peer scoring. A malformed chunk is invalid
                // data (weight -10); a plain failure or timeout is a retrieval
                // failure (weight -2).
                warn!(%peer, %address, %error, ?kind, "Retrieval failed");
                let event = match kind {
                    FailureKind::InvalidChunk => {
                        metrics::counter!(
                            "swarm.client.invalid_chunk",
                            "protocol" => "retrieval",
                        )
                        .increment(1);
                        SwarmScoringEvent::InvalidData
                    }
                    FailureKind::Protocol => SwarmScoringEvent::RetrievalFailure,
                };
                self.report(&peer, event, RETRIEVAL_SOURCE);
            }

            ClientEvent::PushFailed {
                peer,
                address,
                error,
                kind,
            } => {
                // The pusher is resolved directly by the handler; this event
                // exists for peer scoring. Same classification as retrieval.
                warn!(%peer, %address, %error, ?kind, "Push failed");
                let event = match kind {
                    FailureKind::InvalidChunk => {
                        metrics::counter!(
                            "swarm.client.invalid_chunk",
                            "protocol" => "pushsync",
                        )
                        .increment(1);
                        SwarmScoringEvent::InvalidData
                    }
                    FailureKind::Protocol => SwarmScoringEvent::PushFailure,
                };
                self.report(&peer, event, PUSHSYNC_SOURCE);
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

    use alloy_primitives::Signature;
    use nectar_primitives::Nonce;
    use vertex_swarm_primitives::StorageRadius;

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

    fn dummy_signature() -> Signature {
        Signature::new(Default::default(), Default::default(), false)
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
    fn plain_retrieval_failure_reports_retrieval_failure() {
        let (service, reporter) = service_with_reporter();
        service.process_event(ClientEvent::RetrievalFailed {
            peer: peer(2),
            address: ChunkAddress::zero(),
            error: "not found".into(),
            kind: FailureKind::Protocol,
        });
        let (_, event, source) = reporter.single();
        assert_eq!(event, SwarmScoringEvent::RetrievalFailure);
        assert_eq!(source, ReportSource::Protocol("retrieval"));
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
    fn plain_push_failure_reports_push_failure() {
        let (service, reporter) = service_with_reporter();
        service.process_event(ClientEvent::PushFailed {
            peer: peer(4),
            address: ChunkAddress::zero(),
            error: "rejected".into(),
            kind: FailureKind::Protocol,
        });
        let (_, event, _) = reporter.single();
        assert_eq!(event, SwarmScoringEvent::PushFailure);
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
            signature: dummy_signature(),
            nonce: Nonce::from([0u8; 32]),
            storage_radius: StorageRadius::ZERO,
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
