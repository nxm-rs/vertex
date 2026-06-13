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
use crate::serving::SwarmServing;

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
    /// Optional storer-side serving seam. A client node has no storage and
    /// leaves this `None`: it fails every inbound retrieval and push. A storer
    /// node wires a serving seam so it can answer retrievals from its local
    /// store and take custody of pushed chunks for which it is responsible.
    serving: Option<Arc<dyn SwarmServing>>,
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
            serving: None,
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
            serving: None,
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

    /// Attach a storer-side serving seam so the node answers inbound retrievals
    /// from its local store and takes custody of pushed chunks.
    ///
    /// Client nodes never call this: with no serving seam, every inbound
    /// retrieval and push fails (the substream is reset). Storer nodes wire a
    /// [`LocalServing`](crate::serving::LocalServing).
    #[must_use]
    pub fn with_local_store(mut self, serving: Arc<dyn SwarmServing>) -> Self {
        self.serving = Some(serving);
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

    /// Serve an inbound retrieval request from the local store.
    ///
    /// On a hit the stamped chunk is issued through the [`ClientCommand::ServeChunk`]
    /// command, which still passes the handler's verify-before-serve gate (the
    /// served chunk's address must answer the request). A miss, or no serving
    /// seam (a client node), fails the request: the handler resets the request
    /// substream, which the requester reads as a retrieval failure.
    fn serve_retrieval(
        &self,
        peer: OverlayAddress,
        peer_id: libp2p::PeerId,
        address: ChunkAddress,
        request_id: u64,
    ) {
        if let Some(serving) = &self.serving
            && let Some(chunk) = serving.serve(&address)
        {
            debug!(%peer, %address, %request_id, "Serving chunk from local store");
            metrics::counter!("swarm.client.serve_hit").increment(1);
            if let Err(e) = self.handle.send_command(ClientCommand::ServeChunk {
                peer,
                request_id,
                address,
                chunk,
            }) {
                warn!(%peer, %peer_id, error = ?e, "Failed to serve chunk");
            }
            return;
        }

        // Miss, or no local store: fail the request by resetting the substream.
        //
        // TODO(#291): forward a local-store miss to a closer peer instead of
        // failing it outright, then relay the delivery back to the requester.
        debug!(%peer, %address, %request_id, "Retrieval miss, failing request");
        metrics::counter!("swarm.client.serve_miss").increment(1);
        if let Err(e) = self
            .handle
            .send_command(ClientCommand::FailRetrieval { peer, request_id })
        {
            warn!(%peer, %peer_id, error = ?e, "Failed to signal retrieval failure");
        }
    }

    /// Accept an inbound chunk push: validate the stamp, store if responsible,
    /// and return a signed statement-of-custody receipt.
    ///
    /// If we are not responsible (or have no serving seam), the request is
    /// failed: the handler resets the request substream, which the pusher reads
    /// as a push failure.
    fn accept_push(
        &self,
        peer: OverlayAddress,
        peer_id: libp2p::PeerId,
        address: ChunkAddress,
        chunk: StampedChunk,
        request_id: u64,
    ) {
        let Some(serving) = &self.serving else {
            // No local store (a client node): we never take custody.
            //
            // TODO(#291): relay the push onward to a responsible peer instead of
            // failing it outright.
            debug!(%peer, %address, %request_id, "No local store, failing push");
            self.fail_push(peer, peer_id, request_id);
            return;
        };

        if !serving.is_responsible_for(&address) {
            // Not in our area of responsibility.
            //
            // TODO(#291): relay the push onward to a responsible peer instead of
            // failing it outright.
            debug!(%peer, %address, %request_id, "Not responsible for chunk, failing push");
            metrics::counter!("swarm.client.push_not_responsible").increment(1);
            self.fail_push(peer, peer_id, request_id);
            return;
        }

        // Validate the postage stamp. The stamp is the proof of payment that
        // authorizes storage. Here we do the signature/structural check: the
        // stamp must recover a signer over its (chunk address, batch, index,
        // timestamp) digest. A stamp that does not recover is rejected as
        // invalid data.
        //
        // TODO(#76): the full check also requires the recovered signer to be
        // the on-chain owner of an existing, funded batch with sufficient depth
        // and a non-exhausted bucket. That batch state is not available until
        // the postage/storer batch store is wired, so it is deferred here. We
        // do not invent batch state.
        if let Err(e) = chunk.stamp().recover_signer(&address) {
            warn!(%peer, %address, %request_id, error = ?e, "Rejecting push: invalid stamp");
            metrics::counter!(
                "swarm.client.invalid_chunk",
                "protocol" => "pushsync",
            )
            .increment(1);
            self.report(&peer, SwarmScoringEvent::InvalidData, PUSHSYNC_SOURCE);
            self.fail_push(peer, peer_id, request_id);
            return;
        }

        // Take custody by storing the chunk locally.
        if !serving.store(&chunk) {
            warn!(%peer, %address, %request_id, "Failed to store pushed chunk, failing push");
            self.fail_push(peer, peer_id, request_id);
            return;
        }

        // Sign a statement-of-custody receipt and return it. The receipt signs
        // the chunk address so the pusher can recover our signer and confirm a
        // responsible storer took custody.
        let Some(parts) = serving.sign_receipt(&address) else {
            warn!(%peer, %address, %request_id, "Failed to sign receipt, failing push");
            self.fail_push(peer, peer_id, request_id);
            return;
        };

        debug!(%peer, %address, %request_id, "Stored chunk, returning receipt");
        metrics::counter!("swarm.client.push_stored").increment(1);
        if let Err(e) = self.handle.send_command(ClientCommand::SendReceipt {
            peer,
            request_id,
            address,
            signature: parts.signature,
            nonce: parts.nonce,
            storage_radius: parts.storage_radius,
        }) {
            warn!(%peer, %peer_id, error = ?e, "Failed to send receipt");
        }
    }

    /// Fail an inbound push by resetting its substream.
    fn fail_push(&self, peer: OverlayAddress, peer_id: libp2p::PeerId, request_id: u64) {
        if let Err(e) = self
            .handle
            .send_command(ClientCommand::FailPush { peer, request_id })
        {
            warn!(%peer, %peer_id, error = ?e, "Failed to signal push failure");
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
                self.serve_retrieval(peer, peer_id, address, request_id);
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
                self.accept_push(peer, peer_id, address, chunk, request_id);
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

    // --- Serving seam: serve, store-and-receipt, and clean failure ---

    use std::collections::HashMap;

    use alloy_primitives::B256;
    use alloy_signer::SignerSync;
    use nectar_postage::{Stamp, StampDigest, StampIndex};
    use nectar_primitives::{AnyChunk, ContentChunk};
    use vertex_swarm_api::{SwarmIdentity, SwarmLocalStore, SwarmResult};
    use vertex_swarm_test_utils::{MockIdentity, test_peer_id};

    use crate::protocol::ClientCommand;
    use crate::serving::LocalServing;

    /// In-memory [`SwarmLocalStore`] that preserves stamps, so it can answer
    /// stamped retrievals the way the storer's `LocalStoreImpl` does.
    #[derive(Default)]
    struct MapStore {
        chunks: Mutex<HashMap<ChunkAddress, StampedChunk>>,
    }

    impl SwarmLocalStore for MapStore {
        fn store(&self, chunk: &AnyChunk) -> SwarmResult<()> {
            let stamp = Stamp::new(B256::repeat_byte(0xaa), 0, 0, 0, dummy_signature());
            let stamped = StampedChunk::new(chunk.clone(), stamp);
            self.chunks
                .lock()
                .unwrap()
                .insert(*chunk.address(), stamped);
            Ok(())
        }

        fn store_stamped(&self, chunk: &StampedChunk) -> SwarmResult<()> {
            self.chunks
                .lock()
                .unwrap()
                .insert(*chunk.address(), chunk.clone());
            Ok(())
        }

        fn retrieve(&self, address: &ChunkAddress) -> SwarmResult<Option<AnyChunk>> {
            Ok(self
                .chunks
                .lock()
                .unwrap()
                .get(address)
                .map(|c| c.chunk().clone()))
        }

        fn retrieve_stamped(&self, address: &ChunkAddress) -> SwarmResult<Option<StampedChunk>> {
            Ok(self.chunks.lock().unwrap().get(address).cloned())
        }

        fn has(&self, address: &ChunkAddress) -> bool {
            self.chunks.lock().unwrap().contains_key(address)
        }

        fn remove(&self, address: &ChunkAddress) -> SwarmResult<()> {
            self.chunks.lock().unwrap().remove(address);
            Ok(())
        }
    }

    /// A content chunk with a stamp validly signed by `identity`, so the stamp
    /// recovers a real signer (the structural stamp check passes).
    fn signed_stamped_chunk(identity: &MockIdentity, payload: &[u8]) -> StampedChunk {
        let chunk: AnyChunk = ContentChunk::new(payload.to_vec())
            .expect("valid content chunk")
            .into();
        let address = *chunk.address();
        let batch = B256::repeat_byte(0x11);
        let index = StampIndex::new(0, 0);
        let timestamp = 0u64;
        let digest = StampDigest::new(address, batch, index, timestamp);
        let sig = identity
            .signer()
            .sign_message_sync(digest.to_prehash().as_slice())
            .expect("sign stamp");
        let stamp = Stamp::with_index(batch, index, timestamp, sig);
        StampedChunk::new(chunk, stamp)
    }

    /// Build a serving service over `store` and `identity` with an owned command
    /// receiver so a test can observe the commands the handlers emit.
    fn serving_service(
        store: Arc<MapStore>,
        identity: MockIdentity,
    ) -> (ClientService, mpsc::Receiver<ClientCommand>) {
        let (command_tx, command_rx) = mpsc::channel(DEFAULT_CHANNEL_CAPACITY);
        let (_event_tx, event_rx) = mpsc::channel(DEFAULT_CHANNEL_CAPACITY);
        let (service, _handle) = ClientService::with_channels(command_tx, event_rx);
        let serving = Arc::new(LocalServing::new(
            identity,
            store as Arc<dyn SwarmLocalStore>,
            StorageRadius::ZERO,
        ));
        (service.with_local_store(serving), command_rx)
    }

    /// Build a storeless service with an owned command receiver.
    fn storeless_service() -> (ClientService, mpsc::Receiver<ClientCommand>) {
        let (command_tx, command_rx) = mpsc::channel(DEFAULT_CHANNEL_CAPACITY);
        let (_event_tx, event_rx) = mpsc::channel(DEFAULT_CHANNEL_CAPACITY);
        let (service, _handle) = ClientService::with_channels(command_tx, event_rx);
        (service, command_rx)
    }

    #[test]
    fn storer_serves_a_chunk_it_has_stored() {
        let identity = MockIdentity::with_first_byte(0x00);
        let store = Arc::new(MapStore::default());
        let chunk = signed_stamped_chunk(&identity, b"served payload");
        let address = *chunk.address();
        store.store_stamped(&chunk).unwrap();

        let (service, mut command_rx) = serving_service(Arc::clone(&store), identity);
        service.process_event(ClientEvent::ChunkRequested {
            peer: peer(1),
            peer_id: test_peer_id(1),
            address,
            request_id: 7,
        });

        match command_rx.try_recv().expect("a command was emitted") {
            ClientCommand::ServeChunk {
                peer: p,
                request_id,
                address: a,
                chunk: served,
            } => {
                assert_eq!(p, peer(1));
                assert_eq!(request_id, 7);
                assert_eq!(a, address);
                assert_eq!(*served.address(), address);
            }
            other => panic!("expected ServeChunk, got {other:?}"),
        }
    }

    #[test]
    fn storer_accepts_a_push_stores_it_and_returns_a_verifiable_receipt() {
        let identity = MockIdentity::with_first_byte(0x00);
        let expected_signer = identity.ethereum_address();
        let nonce = identity.nonce();
        let store = Arc::new(MapStore::default());
        let chunk = signed_stamped_chunk(&identity, b"pushed payload");
        let address = *chunk.address();

        let (service, mut command_rx) = serving_service(Arc::clone(&store), identity);
        service.process_event(ClientEvent::ChunkPushReceived {
            peer: peer(2),
            peer_id: test_peer_id(1),
            address,
            chunk,
            request_id: 9,
        });

        // The chunk must have been taken into custody.
        assert!(store.has(&address), "chunk must be stored");

        match command_rx.try_recv().expect("a command was emitted") {
            ClientCommand::SendReceipt {
                peer: p,
                request_id,
                address: a,
                signature,
                nonce: receipt_nonce,
                storage_radius,
            } => {
                assert_eq!(p, peer(2));
                assert_eq!(request_id, 9);
                assert_eq!(a, address);
                assert_eq!(receipt_nonce, nonce);
                assert_eq!(storage_radius, StorageRadius::ZERO);
                // Wire-compat: the receipt signs the raw chunk address under the
                // EIP-191 prefix, so a reference peer recovers our signer from
                // the address bytes. Recover and check it matches our identity.
                let recovered = signature
                    .recover_address_from_msg(address.as_slice())
                    .expect("receipt signature recovers");
                assert_eq!(recovered, expected_signer);
            }
            other => panic!("expected SendReceipt, got {other:?}"),
        }
    }

    #[test]
    fn client_node_fails_a_serve_cleanly() {
        let (service, mut command_rx) = storeless_service();
        service.process_event(ClientEvent::ChunkRequested {
            peer: peer(3),
            peer_id: test_peer_id(1),
            address: ChunkAddress::zero(),
            request_id: 1,
        });

        match command_rx.try_recv().expect("a command was emitted") {
            ClientCommand::FailRetrieval {
                peer: p,
                request_id,
            } => {
                assert_eq!(p, peer(3));
                assert_eq!(request_id, 1);
            }
            other => panic!("expected FailRetrieval, got {other:?}"),
        }
    }

    #[test]
    fn client_node_fails_a_push_cleanly() {
        let identity = MockIdentity::with_first_byte(0x00);
        let chunk = signed_stamped_chunk(&identity, b"unwanted payload");
        let address = *chunk.address();

        let (service, mut command_rx) = storeless_service();
        service.process_event(ClientEvent::ChunkPushReceived {
            peer: peer(4),
            peer_id: test_peer_id(1),
            address,
            chunk,
            request_id: 2,
        });

        match command_rx.try_recv().expect("a command was emitted") {
            ClientCommand::FailPush {
                peer: p,
                request_id,
            } => {
                assert_eq!(p, peer(4));
                assert_eq!(request_id, 2);
            }
            other => panic!("expected FailPush, got {other:?}"),
        }
    }
}
