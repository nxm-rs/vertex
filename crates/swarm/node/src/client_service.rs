//! Client service for managing network interactions.
//!
//! The `ClientService` bridges the business logic layer with the network layer.
//! It owns channels to communicate with `ClientBehaviour` and processes
//! incoming events.

use nectar_primitives::ChunkAddress;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};
use vertex_swarm_api::PushReceipt;
use vertex_swarm_net_pseudosettle::PaymentAck;
use vertex_swarm_primitives::{OverlayAddress, StampedChunk};
use vertex_tasks::{GracefulShutdown, SpawnableTask};

use crate::protocol::{ClientCommand, ClientEvent};

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

/// Error from retrieval operations.
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum RetrievalError {
    /// Channel closed.
    #[error("Network channel closed")]
    ChannelClosed,
    /// The target peer has no active connection.
    #[error("Peer not connected")]
    NotConnected,
    /// Request cancelled.
    #[error("Request cancelled")]
    Cancelled,
    /// Protocol error.
    #[error("Protocol error: {0}")]
    Protocol(String),
    /// Chunk not found.
    #[error("Chunk not found: {0}")]
    NotFound(ChunkAddress),
    /// The storer rejected the chunk. Carries the storer's wire error string.
    #[error("Push rejected by storer: {0}")]
    PushRejected(String),
}

impl ClientHandle {
    /// Create a new client handle.
    pub fn new(command_tx: mpsc::Sender<ClientCommand>) -> Self {
        Self { command_tx }
    }

    /// Send a command to the network layer (non-blocking).
    ///
    /// Uses `try_send` because callers (e.g. the libp2p event loop) must not block.
    pub fn send_command(&self, command: ClientCommand) -> Result<(), RetrievalError> {
        self.command_tx.try_send(command).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => {
                warn!("Client command channel full");
                metrics::counter!("swarm.client.commands_dropped").increment(1);
                RetrievalError::ChannelClosed
            }
            mpsc::error::TrySendError::Closed(_) => RetrievalError::ChannelClosed,
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
    ) -> Result<RetrievalResult, RetrievalError> {
        let (tx, rx) = oneshot::channel();

        self.send_command(ClientCommand::RetrieveChunk {
            peer,
            address,
            response: tx,
        })?;

        rx.await.map_err(|_| RetrievalError::Cancelled)?
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
    ) -> Result<PushReceipt, RetrievalError> {
        let address = *chunk.address();
        let (tx, rx) = oneshot::channel();

        self.send_command(ClientCommand::PushChunk {
            peer,
            address,
            chunk,
            response: tx,
        })?;

        rx.await.map_err(|_| RetrievalError::Cancelled)?
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
        };

        (service, handle)
    }

    /// Get a handle for sending commands.
    pub fn handle(&self) -> ClientHandle {
        self.handle.clone()
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
            } => {
                // The requester is resolved directly by the handler; this
                // event exists for accounting and peer scoring.
                debug!(%peer, %address, "Chunk received");
                // TODO: Record bandwidth usage and report RetrievalSuccess
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
            } => {
                // The pusher is resolved directly by the handler; this event
                // exists for accounting and peer scoring.
                debug!(
                    %peer, %address, %storage_radius, %nonce,
                    sig = %signature, "Receipt received"
                );
                // TODO: Record bandwidth usage and report PushSuccess
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
                // The requester is resolved directly by the handler; this
                // event exists for peer scoring.
                warn!(%peer, %address, %error, "Retrieval failed");
                // TODO: Report RetrievalFailure to peer scoring
            }

            ClientEvent::PushFailed {
                peer,
                address,
                error,
            } => {
                // The pusher is resolved directly by the handler; this event
                // exists for peer scoring.
                warn!(%peer, %address, %error, "Push failed");
                // TODO: Report PushFailure to peer scoring
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
