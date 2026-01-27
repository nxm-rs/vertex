//! Client service for managing network interactions.
//!
//! The `ClientService` bridges the business logic layer with the network layer.
//! It owns channels to communicate with `SwarmClientBehaviour` and processes
//! incoming events.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};
use vertex_primitives::{ChunkAddress, OverlayAddress};
use vertex_swarm_api::SpawnableTask;

// Re-export the types from net/client
pub use vertex_net_client::{Cheque, ClientCommand, ClientEvent};

/// Handle for sending commands to the network layer.
#[derive(Clone)]
pub struct ClientHandle {
    command_tx: mpsc::UnboundedSender<ClientCommand>,
    pending_retrievals: Arc<Mutex<HashMap<ChunkAddress, oneshot::Sender<RetrievalResult>>>>,
}

/// Result of a chunk retrieval.
pub struct RetrievalResult {
    /// The chunk data.
    pub data: bytes::Bytes,
    /// The postage stamp.
    pub stamp: bytes::Bytes,
    /// The peer that served the chunk.
    pub peer: OverlayAddress,
}

/// Error from retrieval operations.
#[derive(Debug, thiserror::Error)]
pub enum RetrievalError {
    /// Channel closed.
    #[error("Network channel closed")]
    ChannelClosed,
    /// Request cancelled.
    #[error("Request cancelled")]
    Cancelled,
    /// Protocol error.
    #[error("Protocol error: {0}")]
    Protocol(String),
    /// Chunk not found.
    #[error("Chunk not found: {0}")]
    NotFound(ChunkAddress),
}

impl ClientHandle {
    /// Create a new client handle.
    pub fn new(command_tx: mpsc::UnboundedSender<ClientCommand>) -> Self {
        Self {
            command_tx,
            pending_retrievals: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Send a command to the network layer.
    pub fn send_command(&self, command: ClientCommand) -> Result<(), RetrievalError> {
        self.command_tx
            .send(command)
            .map_err(|_| RetrievalError::ChannelClosed)
    }

    /// Retrieve a chunk from a specific peer.
    ///
    /// This sends a retrieval command and waits for the response.
    pub async fn retrieve_chunk(
        &self,
        peer: OverlayAddress,
        address: ChunkAddress,
    ) -> Result<RetrievalResult, RetrievalError> {
        let (tx, rx) = oneshot::channel();

        // Register the pending retrieval
        {
            let mut pending = self.pending_retrievals.lock();
            pending.insert(address, tx);
        }

        // Send the retrieval command
        self.send_command(ClientCommand::RetrieveChunk { peer, address })?;

        // Wait for the response
        rx.await.map_err(|_| RetrievalError::Cancelled)
    }

    /// Complete a pending retrieval with a result.
    ///
    /// Called by the event processor when a chunk is received.
    pub(crate) fn complete_retrieval(&self, address: ChunkAddress, result: RetrievalResult) {
        let mut pending = self.pending_retrievals.lock();
        if let Some(tx) = pending.remove(&address) {
            let _ = tx.send(result);
        }
    }

    /// Fail a pending retrieval with an error.
    #[allow(dead_code)]
    pub(crate) fn fail_retrieval(&self, address: ChunkAddress, _error: String) {
        let mut pending = self.pending_retrievals.lock();
        pending.remove(&address);
        // The oneshot will be dropped, causing the receiver to get Cancelled
    }
}

/// Client service that processes network events.
///
/// This is the business logic layer that handles `ClientEvent` from the network.
pub struct ClientService {
    /// Handle for sending commands.
    handle: ClientHandle,
    /// Event receiver from the network.
    event_rx: mpsc::UnboundedReceiver<ClientEvent>,
}

impl ClientService {
    /// Create a new client service.
    ///
    /// Returns the service and a sender for events (to be used by the network layer).
    pub fn new() -> (Self, mpsc::UnboundedSender<ClientEvent>, ClientHandle) {
        let (command_tx, _command_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        let handle = ClientHandle::new(command_tx);

        let service = Self {
            handle: handle.clone(),
            event_rx,
        };

        (service, event_tx, handle)
    }

    /// Create with explicit channels.
    ///
    /// Use this when the network layer creates the command channel.
    pub fn with_channels(
        command_tx: mpsc::UnboundedSender<ClientCommand>,
        event_rx: mpsc::UnboundedReceiver<ClientEvent>,
    ) -> (Self, ClientHandle) {
        let handle = ClientHandle::new(command_tx);

        let service = Self {
            handle: handle.clone(),
            event_rx,
        };

        (service, handle)
    }

    /// Get a handle for sending commands.
    pub fn handle(&self) -> ClientHandle {
        self.handle.clone()
    }

    /// Consume self and run as a spawnable future.
    ///
    /// This is the preferred entry point for spawning the service as a background task.
    /// It's suitable for `TaskExecutor::spawn_critical()`.
    pub async fn into_task(self) {
        self.run().await;
    }

    /// Run the event processing loop.
    ///
    /// This should be spawned as a background task.
    /// Prefer [`into_task()`](Self::into_task) for consistency with `SwarmNode`.
    pub async fn run(mut self) {
        while let Some(event) = self.event_rx.recv().await {
            self.process_event(event);
        }
        debug!("Client service shutting down");
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
                data,
                stamp,
            } => {
                debug!(%peer, %address, data_len = data.len(), "Chunk received");
                self.handle
                    .complete_retrieval(address, RetrievalResult { data, stamp, peer });
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
                data,
                stamp,
                request_id,
            } => {
                debug!(
                    %peer_id, %peer, %address, %request_id,
                    data_len = data.len(), stamp_len = stamp.len(),
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
            } => {
                debug!(
                    %peer, %address, %storage_radius,
                    sig_len = signature.len(), nonce_len = nonce.len(),
                    "Receipt received"
                );
                // TODO: Verify receipt, complete push operation
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
            } => {
                warn!(%peer, %address, %error, "Retrieval failed");
                // TODO: Notify waiting retrieval request
                self.handle.fail_retrieval(address, error);
            }

            ClientEvent::PushFailed {
                peer,
                address,
                error,
            } => {
                warn!(%peer, %address, %error, "Push failed");
                // TODO: Notify waiting push request
            }

            ClientEvent::SettlementNeeded { peer, balance } => {
                debug!(%peer, %balance, "Settlement needed");
                // TODO: Initiate settlement (swap or pseudosettle)
            }

            ClientEvent::ChequeReceived {
                peer,
                beneficiary,
                chequebook,
                cumulative_payout,
                signature,
            } => {
                debug!(
                    %peer,
                    beneficiary_len = beneficiary.len(),
                    chequebook_len = chequebook.len(),
                    payout_len = cumulative_payout.len(),
                    sig_len = signature.len(),
                    "Cheque received"
                );
                // TODO: Validate and cash cheque
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
    fn spawn_task(self) -> impl std::future::Future<Output = ()> + Send {
        self.into_task()
    }
}
