//! Connection handler for client protocols.
//!
//! The `ClientHandler` manages multiple protocols on a single connection:
//! - Pricing: Payment threshold exchange
//! - Retrieval: Chunk request/response
//! - PushSync: Chunk push with receipt
//! - Pseudosettle: Bandwidth accounting payments
//!
//! # Lifecycle
//!
//! 1. Handler starts in `Dormant` state when connection established
//! 2. After handshake, `Activate` command transitions to `Active` state
//! 3. In `Active` state, handler processes protocol messages
//!
//! # Inbound serving
//!
//! Retrieval and pushsync inbound requests are served by **self-contained
//! futures**: each request becomes one future pushed into [`ClientHandler`]'s
//! `inbound` set, with the substream's responder as the correlation. A retrieval
//! serves from the cache (content chunks indefinitely, single-owner chunks while
//! fresh) or forwards to a closer peer; a pushsync forwards and relays the
//! storer's receipt verbatim, never signing. The future resolves to an
//! [`InboundOutcome`] that the handler turns into a scoring or metrics event;
//! the response itself never travels back as a command.
//!
//! Pseudosettle inbound still uses the request-id responder map below, because
//! the pseudosettle service gates the accepted amount against a time-based
//! allowance before acking, so its ack cannot be folded inline.

use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use vertex_util_runtime::time::Instant;

use alloy_primitives::U256;
use futures::future::BoxFuture;
use futures::stream::{FuturesUnordered, StreamExt};
use futures_bounded::Timeout;
use libp2p::swarm::{
    SubstreamProtocol,
    handler::{
        ConnectionEvent, ConnectionHandler, ConnectionHandlerEvent, FullyNegotiatedInbound,
        FullyNegotiatedOutbound,
    },
};
use nectar_primitives::{AnyChunk, ChunkAddress, NetworkId};
use tracing::{debug, warn};
use vertex_swarm_api::SwarmLocalStore;
use vertex_swarm_net_pseudosettle::PaymentAck;
use vertex_swarm_net_pushsync::Receipt;
#[cfg(feature = "swap")]
use vertex_swarm_net_swap::SignedCheque;
use vertex_swarm_primitives::{CachedChunk, OverlayAddress, Stamp, StampedChunk, SwarmNodeType};

use super::events::{PushResponseTx, RetrievalResponseTx};
use super::forward::Forwarder;
use super::storer::StorerCapability;
use super::upgrade::{
    ClientInboundOutput, ClientInboundUpgrade, ClientOutboundInfo, ClientOutboundOutput,
    ClientOutboundUpgrade, ClientUpgradeError, FailureKind,
};
use crate::client_service::{ChunkTransferError, RetrievalResult};
use vertex_swarm_net_pushsync::PROTOCOL_NAME as PUSHSYNC_PROTOCOL;
use vertex_swarm_net_retrieval::PROTOCOL_NAME as RETRIEVAL_PROTOCOL;

const DEFAULT_MAX_PENDING_COMMANDS: usize = 256;
const DEFAULT_MAX_PENDING_EVENTS: usize = 256;
/// Maximum number of stored pseudosettle responders per connection.
const MAX_PENDING_RESPONSES: usize = 64;
/// Timeout for async response sending (prevent stuck streams).
const RESPONSE_SEND_TIMEOUT: Duration = Duration::from_secs(15);
/// Maximum concurrent response sends per connection.
const MAX_CONCURRENT_RESPONSE_SENDS: usize = 8;
/// Responders older than this are dropped as stale.
const RESPONDER_STALE_TIMEOUT: Duration = Duration::from_secs(60);
/// Maximum concurrent inbound serving futures per connection.
///
/// Caps how many retrieval and pushsync requests a single peer can have in
/// flight against us at once; once full, `listen_protocol` stops advertising
/// inbound serving so the muxer back-pressures the peer.
const MAX_INBOUND_SERVING: usize = 32;

/// The outcome of serving one inbound retrieval or pushsync request.
///
/// Returned by the self-contained inbound future and turned into a scoring or
/// metrics event by the handler. It never carries a response: the response was
/// already sent (or the substream reset) inside the future.
#[derive(Debug)]
pub(crate) enum InboundOutcome {
    /// A retrieval was answered from our cache.
    Served { overlay: OverlayAddress },
    /// A retrieval was answered by forwarding to a closer peer.
    Forwarded { overlay: OverlayAddress },
    /// A retrieval could not be served or forwarded; the substream was reset.
    Missed {
        overlay: OverlayAddress,
        address: ChunkAddress,
    },
    /// A pushsync was forwarded and the storer's receipt relayed verbatim.
    Relayed { overlay: OverlayAddress },
    /// An inbound pushsync the node is responsible for was stored into the
    /// reserve and acknowledged with a freshly signed custody receipt.
    Stored { overlay: OverlayAddress },
    /// A pushsync could not be forwarded (or, on the storer ingest path, could
    /// not be stored or acknowledged); the substream was reset.
    PushFailed {
        overlay: OverlayAddress,
        address: ChunkAddress,
    },
}

/// Configuration for the client handler.
///
/// # Deadlines are split deliberately
///
/// Three deadlines coexist here, and the split is intentional. `timeout` is the
/// shared deadline for pricing, pseudosettle, and swap, where no per-caller
/// liveness guarantee is owed. `retrieval_timeout` and `pushsync_timeout` are
/// the named, per-protocol deadlines for the two chunk-transfer operations, each
/// threaded into its own outbound [`SubstreamProtocol`].
///
/// They default to the same 30s value but are separate fields on purpose: the
/// retrieval and pushsync liveness invariant (below) is bounded by these two and
/// nothing else, so tuning one must not silently move pricing, pseudosettle, or
/// swap. Do not collapse the three back into one field assuming they are equal;
/// a future change may shorten `retrieval_timeout` for a faster candidate-race
/// without touching settlement.
///
/// # Retrieval and pushsync liveness invariant
///
/// A peer that negotiates the chunk-transfer substream and its headers but then
/// withholds the response frame (a delivery for retrieval, a receipt for
/// pushsync) cannot stall the caller indefinitely. The outbound
/// [`SubstreamProtocol`] is dispatched `.with_timeout(retrieval_timeout)` (or
/// `pushsync_timeout`), so the upgrade future, including the blocked read of the
/// response frame, is bounded by that deadline. When it elapses the attempt
/// resolves with [`ChunkTransferError::TimedOut`] and the caller is free to race
/// the next candidate. This is the only liveness boundary against a withholding
/// peer, so the deadline lives on a named field rather than an incidental shared
/// value.
#[derive(Debug, Clone)]
pub(crate) struct Config {
    /// Shared deadline for the protocols with no per-caller liveness guarantee:
    /// pricing, pseudosettle, and swap. Retrieval and pushsync use their own
    /// named deadlines below; see the type-level note on the deliberate split.
    pub(crate) timeout: Duration,
    /// Deadline for an outbound retrieval. Bounds the substream upgrade,
    /// including the blocked read of the delivery frame, so a withholding peer
    /// resolves the attempt with [`ChunkTransferError::TimedOut`] rather than
    /// hanging. Deliberately distinct from `timeout` (see the type-level note).
    pub(crate) retrieval_timeout: Duration,
    /// Deadline for an outbound pushsync. Bounds the substream upgrade,
    /// including the blocked read of the receipt frame, so a withholding peer
    /// resolves the attempt with [`ChunkTransferError::TimedOut`] rather than
    /// hanging. Deliberately distinct from `timeout` (see the type-level note).
    pub(crate) pushsync_timeout: Duration,
    /// Maximum pending commands before dropping new ones.
    pub(crate) max_pending_commands: usize,
    /// Maximum pending events before dropping new ones.
    pub(crate) max_pending_events: usize,
    /// Local node's role. Controls which protocols are advertised on
    /// inbound substream upgrades and which outbound commands are honoured.
    /// Bootnodes only speak pricing (listen-only) so the rest of the
    /// client surface is inert.
    pub(crate) local_role: SwarmNodeType,
    /// Network id, used to recover the signer overlay of an inbound custody
    /// receipt at the decode boundary (`compute_overlay(eth, network_id, nonce)`).
    pub(crate) network_id: NetworkId,
    /// Our advertised swap exchange rate, sent in the swap headers exchange.
    /// Rate negotiation is owned by the settlement service; the handler only
    /// carries the value onto the wire.
    #[cfg(feature = "swap")]
    pub(crate) swap_exchange_rate: U256,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            retrieval_timeout: Duration::from_secs(30),
            pushsync_timeout: Duration::from_secs(30),
            max_pending_commands: DEFAULT_MAX_PENDING_COMMANDS,
            max_pending_events: DEFAULT_MAX_PENDING_EVENTS,
            local_role: SwarmNodeType::Client,
            network_id: NetworkId::MAINNET,
            #[cfg(feature = "swap")]
            swap_exchange_rate: U256::ZERO,
        }
    }
}

/// Commands sent from the behaviour to the handler.
///
/// `PushChunk` carries a whole [`StampedChunk`] and dwarfs the other variants;
/// the size difference is accepted rather than boxing a one-shot upload value.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub(crate) enum HandlerCommand {
    /// Activate the handler after handshake completion.
    Activate {
        /// The peer's overlay address.
        overlay: OverlayAddress,
        /// The peer's node type.
        node_type: SwarmNodeType,
    },
    /// Announce our payment threshold to the peer.
    AnnouncePricing {
        /// The threshold to announce.
        threshold: U256,
    },
    /// Request a chunk from the peer.
    RetrieveChunk {
        /// The address of the chunk to retrieve.
        address: ChunkAddress,
        /// Resolves with the retrieved chunk or the failure.
        response: RetrievalResponseTx,
    },
    /// Push a chunk to the peer for storage.
    PushChunk {
        /// The chunk and its postage stamp.
        chunk: StampedChunk,
        /// Resolves with the storer's receipt or the failure.
        response: PushResponseTx,
    },
    /// Send a pseudosettle payment to the peer.
    SendPseudosettle {
        /// The amount to send.
        amount: U256,
    },
    /// Acknowledge a pseudosettle payment.
    AckPseudosettle {
        /// Request ID to match the responder.
        request_id: u64,
        /// The ack to send.
        ack: PaymentAck,
    },
    /// Send a swap cheque to the peer.
    #[cfg(feature = "swap")]
    SendCheque {
        /// The signed cheque to send.
        cheque: SignedCheque,
    },
}

/// Events emitted by the handler to the behaviour.
///
/// `ChunkReceived` carries a whole chunk and dwarfs the other variants; the size
/// difference is accepted rather than boxing a delivery that flows straight to
/// the cache and the requester.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub(crate) enum HandlerEvent {
    /// Handler has been activated.
    Activated {
        /// The peer's overlay address.
        overlay: OverlayAddress,
    },
    /// Received pricing threshold from peer.
    PricingReceived {
        /// The peer's overlay address.
        overlay: OverlayAddress,
        /// The payment threshold.
        threshold: U256,
    },
    /// Successfully sent our pricing threshold.
    PricingSent {
        /// The peer's overlay address.
        overlay: OverlayAddress,
    },
    /// We served an inbound retrieval request from our cache.
    ///
    /// Scoring and metrics only: the chunk has already gone down the wire.
    InboundServed {
        /// The peer we served.
        overlay: OverlayAddress,
    },
    /// We answered an inbound retrieval by forwarding to a closer peer.
    InboundForwarded {
        /// The peer we served.
        overlay: OverlayAddress,
    },
    /// We could not serve or forward an inbound retrieval; the substream reset.
    InboundMissed {
        /// The peer that asked.
        overlay: OverlayAddress,
        /// The requested chunk address.
        address: ChunkAddress,
    },
    /// We relayed a storer's receipt for an inbound pushsync.
    InboundRelayed {
        /// The peer that pushed.
        overlay: OverlayAddress,
    },
    /// We took custody of an inbound pushsync delivery: stored it in the reserve
    /// and acknowledged it with our own signed receipt.
    InboundStored {
        /// The peer that pushed.
        overlay: OverlayAddress,
    },
    /// We could not forward an inbound pushsync; the substream reset.
    InboundPushFailed {
        /// The peer that pushed.
        overlay: OverlayAddress,
        /// The chunk address.
        address: ChunkAddress,
    },
    /// Received a chunk from peer.
    ChunkReceived {
        /// The peer's overlay address.
        overlay: OverlayAddress,
        /// The chunk address.
        address: ChunkAddress,
        /// The received chunk.
        chunk: AnyChunk,
        /// The postage stamp the responder attached, if any.
        stamp: Option<Stamp>,
        /// Time from outbound request to delivery, for latency scoring.
        latency: Duration,
    },
    /// Received a receipt from peer.
    ReceiptReceived {
        /// The peer's overlay address.
        overlay: OverlayAddress,
        /// The chunk address.
        address: ChunkAddress,
        /// Time from outbound request to receipt, for latency scoring.
        latency: Duration,
    },
    /// An outbound retrieval request failed.
    ///
    /// The requester has already been resolved through its response channel;
    /// this event feeds peer scoring and metrics.
    RetrievalFailed {
        /// The peer's overlay address.
        overlay: OverlayAddress,
        /// The requested chunk address.
        address: ChunkAddress,
        /// Error description.
        error: String,
        /// Whether the failure was a malformed chunk (vs a plain failure).
        kind: FailureKind,
    },
    /// An outbound chunk push failed.
    ///
    /// The pusher has already been resolved through its response channel;
    /// this event feeds peer scoring and metrics.
    PushFailed {
        /// The peer's overlay address.
        overlay: OverlayAddress,
        /// The chunk address.
        address: ChunkAddress,
        /// Error description.
        error: String,
        /// Whether the failure was a malformed chunk (vs a plain failure).
        kind: FailureKind,
    },
    /// A peer sent us malformed data on an inbound substream.
    ///
    /// Raised when an inbound pushsync delivery or retrieval request fails
    /// chunk or stamp reconstruction at decode. Attributed to the sender so
    /// the offending peer is scored adversely and the chunk is never relayed.
    InboundInvalidData {
        /// The peer's overlay address.
        overlay: OverlayAddress,
        /// The protocol that rejected the data.
        protocol: &'static str,
    },
    /// Protocol error occurred.
    Error {
        /// The peer's overlay address (if known).
        overlay: Option<OverlayAddress>,
        /// The protocol that errored.
        protocol: &'static str,
        /// Error description.
        error: String,
    },
    /// Received pseudosettle payment from peer.
    PseudosettleReceived {
        /// The peer's overlay address.
        overlay: OverlayAddress,
        /// The payment amount.
        amount: U256,
        /// Request ID for matching ack.
        request_id: u64,
    },
    /// Successfully sent pseudosettle payment.
    PseudosettleSent {
        /// The peer's overlay address.
        overlay: OverlayAddress,
        /// The ack received.
        ack: PaymentAck,
    },
    /// Received a swap cheque from peer.
    #[cfg(feature = "swap")]
    SwapChequeReceived {
        /// The peer's overlay address.
        overlay: OverlayAddress,
        /// The signed cheque received.
        cheque: SignedCheque,
        /// The peer's advertised exchange rate, from the headers exchange.
        peer_rate: U256,
    },
    /// Successfully sent a swap cheque.
    #[cfg(feature = "swap")]
    SwapChequeSent {
        /// The peer's overlay address.
        overlay: OverlayAddress,
        /// The peer's advertised exchange rate, from the headers exchange.
        peer_rate: U256,
    },
}

/// Handler state machine.
#[derive(Debug)]
enum State {
    /// Waiting for activation command.
    Dormant,
    /// Active and processing protocols.
    Active { overlay: OverlayAddress },
}

/// A pending inbound pseudosettle response waiting for the application layer to
/// gate the amount and provide an ack.
struct StoredResponse {
    response: vertex_swarm_net_pseudosettle::PseudosettleInboundResult,
    stored_at: Instant,
}

/// Swarm client connection handler.
///
/// Manages multiple client protocols on a single peer connection.
pub(crate) struct ClientHandler {
    config: Config,
    state: State,
    /// The client cache, shared from the behaviour. Inbound retrievals serve
    /// from it; forwarded retrieval deliveries are cached into it.
    store: Arc<dyn SwarmLocalStore>,
    /// The forwarder seam, shared from the behaviour. A cache miss forwards a
    /// retrieval; a pushsync this node is not responsible for forwards. Stubbed
    /// in the cache-only client.
    forward: Arc<dyn Forwarder>,
    /// The storer ingest capability, present only on a storer node. When set,
    /// an inbound pushsync delivery the node is responsible for is stored into
    /// the reserve and acknowledged with a freshly signed custody receipt; when
    /// absent (a client), every delivery takes the verbatim-relay path.
    storer: Option<StorerCapability>,
    /// Counter for pseudosettle request IDs.
    next_request_id: u64,
    /// Pending commands to process.
    pending_commands: VecDeque<HandlerCommand>,
    /// Pending events to emit.
    pending_events: VecDeque<HandlerEvent>,
    /// Whether pricing has been sent.
    pricing_sent: bool,
    /// Whether pricing outbound is pending.
    pricing_outbound_pending: bool,
    /// Self-contained inbound serving futures (retrieval and pushsync).
    inbound: FuturesUnordered<BoxFuture<'static, InboundOutcome>>,
    /// Stored pseudosettle responders waiting for the service's ack, keyed by
    /// request_id. Retrieval and pushsync no longer use this map; only
    /// pseudosettle does, because its ack is gated on a time-based allowance.
    pending_responses: HashMap<u64, StoredResponse>,
    /// Bounded set for async pseudosettle ack sends (prevents blocking poll).
    response_sends: futures_bounded::FuturesSet<Result<(), String>>,
}

impl ClientHandler {
    /// Push an event if the queue isn't full, otherwise drop with a metric.
    fn push_event(&mut self, event: HandlerEvent) {
        if self.pending_events.len() >= self.config.max_pending_events {
            warn!("Handler event queue full, dropping event");
            metrics::counter!("swarm.client.handler.events_dropped").increment(1);
            return;
        }
        self.pending_events.push_back(event);
    }

    /// Create a new handler in dormant state.
    ///
    /// `storer` is `Some` only on a storer node; a client passes `None` and runs
    /// the verbatim-relay pushsync path unchanged.
    pub(crate) fn new(
        config: Config,
        store: Arc<dyn SwarmLocalStore>,
        forward: Arc<dyn Forwarder>,
        storer: Option<StorerCapability>,
    ) -> Self {
        Self {
            config,
            state: State::Dormant,
            store,
            forward,
            storer,
            next_request_id: 0,
            pending_commands: VecDeque::new(),
            pending_events: VecDeque::new(),
            pricing_sent: false,
            pricing_outbound_pending: false,
            inbound: FuturesUnordered::new(),
            pending_responses: HashMap::new(),
            response_sends: futures_bounded::FuturesSet::new(
                RESPONSE_SEND_TIMEOUT,
                MAX_CONCURRENT_RESPONSE_SENDS,
            ),
        }
    }

    /// Get the overlay address if active.
    fn overlay(&self) -> Option<OverlayAddress> {
        match &self.state {
            State::Active { overlay, .. } => Some(*overlay),
            _ => None,
        }
    }

    /// Generate the next request ID (pseudosettle only).
    fn next_request_id(&mut self) -> u64 {
        let id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1);
        id
    }

    /// Store a pending pseudosettle response, evicting stale entries if at
    /// capacity.
    fn store_response(
        &mut self,
        request_id: u64,
        response: vertex_swarm_net_pseudosettle::PseudosettleInboundResult,
    ) {
        if self.pending_responses.len() >= MAX_PENDING_RESPONSES {
            self.evict_stale_responses();
        }
        if self.pending_responses.len() >= MAX_PENDING_RESPONSES {
            warn!(%request_id, "Pending response map full, dropping oldest");
            metrics::counter!("swarm.client.handler.responses_dropped").increment(1);
            if let Some(&oldest_id) = self
                .pending_responses
                .iter()
                .min_by_key(|(_, v)| v.stored_at)
                .map(|(k, _)| k)
            {
                self.pending_responses.remove(&oldest_id);
            }
        }
        self.pending_responses.insert(
            request_id,
            StoredResponse {
                response,
                stored_at: Instant::now(),
            },
        );
    }

    /// Remove responders older than the stale timeout.
    fn evict_stale_responses(&mut self) {
        let cutoff = Instant::now() - RESPONDER_STALE_TIMEOUT;
        self.pending_responses.retain(|_, v| v.stored_at > cutoff);
    }

    /// Take a pending pseudosettle response by request ID.
    fn take_response(
        &mut self,
        request_id: u64,
    ) -> Option<vertex_swarm_net_pseudosettle::PseudosettleInboundResult> {
        self.pending_responses
            .remove(&request_id)
            .map(|s| s.response)
    }

    /// Process activation command.
    fn activate(&mut self, overlay: OverlayAddress, node_type: SwarmNodeType) {
        match &self.state {
            State::Dormant => {
                debug!(%overlay, ?node_type, "Handler activated");
                self.state = State::Active { overlay };
                self.pending_events
                    .push_back(HandlerEvent::Activated { overlay });
            }
            State::Active { .. } => {
                warn!("Handler already active, ignoring duplicate activation");
            }
        }
    }

    /// Handle incoming pricing threshold.
    fn on_pricing_received(
        &mut self,
        threshold: vertex_swarm_net_pricing::AnnouncePaymentThreshold,
    ) {
        if let Some(overlay) = self.overlay() {
            debug!(%overlay, threshold = %threshold.payment_threshold, "Received pricing");
            self.pending_events
                .push_back(HandlerEvent::PricingReceived {
                    overlay,
                    threshold: threshold.payment_threshold,
                });
        } else {
            warn!(
                threshold = %threshold.payment_threshold,
                "Received pricing in dormant state (peer may have cached old protocol list)"
            );
        }
    }

    /// Handle an inbound retrieval request by pushing a self-contained serving
    /// future: serve from cache (content indefinitely, single-owner while
    /// fresh), else forward to a closer peer, caching a forwarded delivery.
    fn on_retrieval_request(
        &mut self,
        request: vertex_swarm_net_retrieval::Request,
        responder: vertex_swarm_net_retrieval::RetrievalResponder,
    ) {
        let Some(overlay) = self.overlay() else {
            warn!(
                address = %request.address,
                "Received retrieval request in dormant state (peer may have cached old protocol list)"
            );
            return;
        };
        let address = request.address;
        debug!(%overlay, %address, "Received retrieval request");

        let store = Arc::clone(&self.store);
        let forward = Arc::clone(&self.forward);
        self.inbound.push(Box::pin(async move {
            // Cache hit: content chunks serve indefinitely, single-owner chunks
            // only while fresh (the store applies the TTL on `get`). A cached
            // content chunk is stampless, a cached SOC carries its stamp; serve
            // whichever stamp the cache held (matching how it was delivered).
            if let Ok(Some(cached)) = store.get(&address)
                && *cached.address() == address
            {
                let (chunk, stamp) = cached.into_parts();
                match responder.send_chunk(chunk, stamp).await {
                    Ok(()) => return InboundOutcome::Served { overlay },
                    Err(e) => {
                        debug!(%overlay, %address, error = %e, "Cache serve send failed");
                        return InboundOutcome::Missed { overlay, address };
                    }
                }
            }

            // Miss: forward to a closer peer, excluding the requester. The
            // forwarder already validated the chunk against the address; the
            // address equality re-check here is the serve-time guard. A retrieved
            // content chunk (CAC) is immutable, so it is cached by address even
            // when stampless (exactly as a storer answers). A retrieved SOC is
            // never cached: it arrives without a version signal and could serve a
            // stale revision, so it is only relayed onward, never stored.
            match forward.retrieve(address, overlay).await {
                Ok(forwarded) => {
                    if *forwarded.chunk.address() != address {
                        // A forwarder that returns a chunk for the wrong address
                        // is a bug in the relay, not the requester's fault; reset.
                        // Drop the credit unapplied (nothing was delivered).
                        drop(forwarded.provide);
                        responder.send_error();
                        return InboundOutcome::Missed { overlay, address };
                    }
                    if forwarded.chunk.is_content() {
                        let _ = store.put(CachedChunk::new(
                            forwarded.chunk.clone(),
                            forwarded.stamp.clone(),
                        ));
                    }
                    match responder.send_chunk(forwarded.chunk, forwarded.stamp).await {
                        Ok(()) => {
                            // The chunk is on the wire: commit the upstream
                            // credit now (the requester actually received the
                            // delivery we are billing for).
                            forwarded.provide.apply_boxed();
                            InboundOutcome::Forwarded { overlay }
                        }
                        Err(e) => {
                            // The requester never received the chunk: drop the
                            // un-applied credit, releasing the reservation, so
                            // we never bill for a delivery that did not land.
                            debug!(%overlay, %address, error = %e, "Forward serve send failed");
                            drop(forwarded.provide);
                            InboundOutcome::Missed { overlay, address }
                        }
                    }
                }
                Err(_) => {
                    responder.send_error();
                    InboundOutcome::Missed { overlay, address }
                }
            }
        }));
    }

    /// Handle an inbound pushsync delivery.
    ///
    /// A storer that is responsible for the chunk takes custody: it stores the
    /// delivery in its reserve and acknowledges it with its own freshly signed
    /// custody receipt. Otherwise (a client, or a storer not responsible for the
    /// address) the delivery is forwarded to a closer peer and the storer's
    /// receipt relayed verbatim — this node never signs for a chunk it does not
    /// store. A store or forward failure resets the substream.
    fn on_pushsync_delivery(
        &mut self,
        delivery: vertex_swarm_net_pushsync::Delivery,
        responder: vertex_swarm_net_pushsync::PushsyncResponder,
    ) {
        let Some(overlay) = self.overlay() else {
            warn!(
                address = %delivery.chunk.address(),
                "Received pushsync delivery in dormant state (peer may have cached old protocol list)"
            );
            return;
        };
        let chunk = *delivery.chunk;
        let address = *chunk.address();
        debug!(%overlay, %address, "Received pushsync delivery");

        // Storer ingest: if this node holds a reserve and is responsible for the
        // chunk, take custody locally instead of relaying. The capability is
        // absent on a client, so the client path below is unchanged.
        if let Some(storer) = &self.storer
            && storer.reserve.is_responsible_for(&address)
        {
            let storer = storer.clone();
            self.inbound.push(Box::pin(async move {
                Self::store_and_sign(storer, chunk, address, overlay, responder).await
            }));
            return;
        }

        let forward = Arc::clone(&self.forward);
        self.inbound.push(Box::pin(async move {
            match forward.push(chunk, overlay).await {
                Ok(forwarded) => {
                    // Relay the storer's receipt verbatim: we never sign. The
                    // signer was recovered and verified at decode, so the wire
                    // bytes reproduce the storer's own signature, nonce, and
                    // radius unchanged.
                    let relay = forwarded.receipt.to_wire();
                    match responder.send_receipt(relay).await {
                        Ok(()) => {
                            // The receipt reached the pusher: commit the upstream
                            // credit now.
                            forwarded.provide.apply_boxed();
                            InboundOutcome::Relayed { overlay }
                        }
                        Err(e) => {
                            // The pusher never received the receipt: drop the
                            // un-applied credit, releasing the reservation.
                            debug!(%overlay, %address, error = %e, "Receipt relay send failed");
                            drop(forwarded.provide);
                            InboundOutcome::PushFailed { overlay, address }
                        }
                    }
                }
                Err(_) => {
                    responder.send_error();
                    InboundOutcome::PushFailed { overlay, address }
                }
            }
        }));
    }

    /// Take custody of a delivery: store it into the reserve and acknowledge it
    /// with a freshly signed custody receipt.
    ///
    /// Reached only when the node holds a [`StorerCapability`] and is responsible
    /// for `address`. The chunk arrives as an always-stamped [`StampedChunk`], so
    /// it goes into the reserve as a stamped [`CachedChunk`]. A reserve put
    /// failure (a capacity error, or a backend error) or a sign failure resets the
    /// substream rather than acknowledging a chunk we did not durably take.
    async fn store_and_sign(
        storer: StorerCapability,
        chunk: StampedChunk,
        address: ChunkAddress,
        overlay: OverlayAddress,
        responder: vertex_swarm_net_pushsync::PushsyncResponder,
    ) -> InboundOutcome {
        // Persist the delivery before acknowledging it: a receipt must never
        // claim custody of a chunk that is not durably in the reserve. The
        // reserve is always stamped; the pushsync delivery is always stamped, so
        // the conversion preserves the stamp.
        if let Err(e) = storer.reserve.put(CachedChunk::from(chunk)) {
            debug!(%overlay, %address, error = %e, "Reserve put failed; not acknowledging");
            responder.send_error();
            return InboundOutcome::PushFailed { overlay, address };
        }

        // Mint our own custody receipt over the chunk address, declaring our
        // current storage radius. The capability is the node's identity, so it
        // supplies the signing key and the overlay-derivation inputs (network id
        // and nonce); a forwarder upstream recovers our overlay from the
        // signature exactly as it would a relayed receipt.
        let storage_radius = storer.reserve.storage_radius();
        let receipt = match Receipt::sign(&storer, address, storage_radius) {
            Ok(receipt) => receipt,
            Err(e) => {
                // The chunk is stored, but we cannot prove custody. Reset rather
                // than send an unsigned acknowledgement; the pusher retries.
                debug!(%overlay, %address, error = %e, "Receipt sign failed; not acknowledging");
                responder.send_error();
                return InboundOutcome::PushFailed { overlay, address };
            }
        };

        match responder.send_receipt(receipt.to_wire()).await {
            Ok(()) => InboundOutcome::Stored { overlay },
            Err(e) => {
                // The chunk is stored; only the acknowledgement failed to reach
                // the pusher. The pusher treats the reset as a failed push and
                // retries, which is idempotent: the reserve put is
                // content-addressed, so a re-delivery is a no-op.
                debug!(%overlay, %address, error = %e, "Receipt send failed after store");
                InboundOutcome::PushFailed { overlay, address }
            }
        }
    }

    /// Turn a resolved inbound outcome into a scoring or metrics event.
    fn on_inbound_outcome(&mut self, outcome: InboundOutcome) {
        let event = match outcome {
            InboundOutcome::Served { overlay } => HandlerEvent::InboundServed { overlay },
            InboundOutcome::Forwarded { overlay } => HandlerEvent::InboundForwarded { overlay },
            InboundOutcome::Missed { overlay, address } => {
                HandlerEvent::InboundMissed { overlay, address }
            }
            InboundOutcome::Relayed { overlay } => HandlerEvent::InboundRelayed { overlay },
            InboundOutcome::Stored { overlay } => HandlerEvent::InboundStored { overlay },
            InboundOutcome::PushFailed { overlay, address } => {
                HandlerEvent::InboundPushFailed { overlay, address }
            }
        };
        self.push_event(event);
    }

    /// Handle retrieval response, resolving the caller's response channel.
    fn on_retrieval_response(
        &mut self,
        delivery: vertex_swarm_net_retrieval::Delivery,
        address: ChunkAddress,
        response: RetrievalResponseTx,
        latency: Duration,
    ) {
        let overlay = self.overlay();
        match delivery {
            vertex_swarm_net_retrieval::Delivery::Error => {
                // The remote reported a failure (signalled by empty data). The
                // reason is adversarial input we never read; it is a plain
                // protocol failure, not malformed data. Malformed chunks never
                // reach this arm: they fail reconstruction at decode and surface
                // as a dial upgrade error.
                debug!(?overlay, %address, "Retrieval failed");
                if let Some(overlay) = overlay {
                    self.push_event(HandlerEvent::RetrievalFailed {
                        overlay,
                        address,
                        error: "remote reported a failure".to_string(),
                        kind: FailureKind::Protocol,
                    });
                }
                let _ = response.send(Err(ChunkTransferError::Remote));
            }
            vertex_swarm_net_retrieval::Delivery::Chunk { chunk, stamp } => {
                let chunk = *chunk;
                let Some(overlay) = overlay else {
                    let _ = response.send(Err(ChunkTransferError::Protocol(
                        "handler not active".to_string(),
                    )));
                    return;
                };
                debug!(%overlay, %address, "Received chunk");
                self.push_event(HandlerEvent::ChunkReceived {
                    overlay,
                    address,
                    chunk: chunk.clone(),
                    stamp: stamp.clone(),
                    latency,
                });
                let _ = response.send(Ok(RetrievalResult {
                    chunk,
                    stamp,
                    peer: overlay,
                }));
            }
        }
    }

    /// Handle pushsync receipt, resolving the caller's response channel.
    fn on_pushsync_receipt(
        &mut self,
        response_msg: vertex_swarm_net_pushsync::ReceiptResponse,
        address: ChunkAddress,
        response: PushResponseTx,
        latency: Duration,
    ) {
        let overlay = self.overlay();
        match response_msg {
            vertex_swarm_net_pushsync::ReceiptResponse::Failed => {
                // The remote reported a rejection (signalled by an empty
                // signature; the reference does not sign its failures). The
                // reason is adversarial input we never read.
                debug!(?overlay, %address, "Pushsync failed");
                if let Some(overlay) = overlay {
                    self.push_event(HandlerEvent::PushFailed {
                        overlay,
                        address,
                        error: "remote reported a failure".to_string(),
                        kind: FailureKind::Protocol,
                    });
                }
                let _ = response.send(Err(ChunkTransferError::Remote));
            }
            vertex_swarm_net_pushsync::ReceiptResponse::Stored(receipt) => {
                let Some(overlay) = overlay else {
                    let _ = response.send(Err(ChunkTransferError::Protocol(
                        "handler not active".to_string(),
                    )));
                    return;
                };
                let receipt_address = receipt.address;
                // The decode boundary: reconstruct and verify the receipt storer
                // before any domain consumer sees it. A receipt whose storer
                // cannot be recovered (an all-zero or unrecoverable signature) is
                // rejected here as invalid data and never becomes a domain
                // receipt; the peer that handed it back is scored.
                match Receipt::reconstruct(receipt, self.config.network_id) {
                    Ok(receipt) => {
                        debug!(%overlay, address = %receipt_address, "Received receipt");
                        self.push_event(HandlerEvent::ReceiptReceived {
                            overlay,
                            address: receipt_address,
                            latency,
                        });
                        let _ = response.send(Ok(receipt));
                    }
                    Err(err) => {
                        debug!(
                            %overlay,
                            address = %receipt_address,
                            error = <&'static str>::from(&err),
                            "Rejected unrecoverable custody receipt at decode"
                        );
                        self.push_event(HandlerEvent::PushFailed {
                            overlay,
                            address: receipt_address,
                            error: err.to_string(),
                            kind: FailureKind::InvalidChunk,
                        });
                        let _ = response.send(Err(ChunkTransferError::Remote));
                    }
                }
            }
        }
    }
}

/// Multi-protocol ConnectionHandler implementation.
#[allow(deprecated)]
impl ConnectionHandler for ClientHandler {
    type FromBehaviour = HandlerCommand;
    type ToBehaviour = HandlerEvent;
    type InboundProtocol = ClientInboundUpgrade;
    type OutboundProtocol = ClientOutboundUpgrade;
    type InboundOpenInfo = ();
    type OutboundOpenInfo = ClientOutboundInfo;

    fn listen_protocol(&self) -> SubstreamProtocol<Self::InboundProtocol, Self::InboundOpenInfo> {
        // Back-pressure: once the inbound serving set is full, advertise the
        // dormant (empty) protocol set so the muxer stops accepting new inbound
        // retrieval and pushsync substreams from this peer until we drain.
        let upgrade = match &self.state {
            State::Active { .. } if self.inbound.len() < MAX_INBOUND_SERVING => {
                let upgrade = ClientInboundUpgrade::active_for(self.config.local_role);
                #[cfg(feature = "swap")]
                let upgrade = upgrade.with_swap_rate(self.config.swap_exchange_rate);
                upgrade
            }
            State::Active { .. } | State::Dormant => ClientInboundUpgrade::new(),
        };
        SubstreamProtocol::new(upgrade, ()).with_timeout(self.config.timeout)
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<
        ConnectionHandlerEvent<Self::OutboundProtocol, Self::OutboundOpenInfo, Self::ToBehaviour>,
    > {
        // Emit pending events first
        if let Some(event) = self.pending_events.pop_front() {
            return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(event));
        }

        // Drain resolved inbound serving futures into scoring/metrics events.
        while let Poll::Ready(Some(outcome)) = self.inbound.poll_next_unpin(cx) {
            self.on_inbound_outcome(outcome);
            if let Some(event) = self.pending_events.pop_front() {
                return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(event));
            }
        }

        // Drain completed pseudosettle ack sends
        while let Poll::Ready(result) = self.response_sends.poll_unpin(cx) {
            match result {
                Ok(Ok(())) => {
                    debug!("Response send completed");
                }
                Ok(Err(err)) => {
                    warn!(error = %err, "Response send failed");
                    self.push_event(HandlerEvent::Error {
                        overlay: self.overlay(),
                        protocol: "response",
                        error: err,
                    });
                }
                Err(Timeout { .. }) => {
                    warn!("Response send timed out");
                    self.push_event(HandlerEvent::Error {
                        overlay: self.overlay(),
                        protocol: "response",
                        error: "response send timed out".into(),
                    });
                }
            }
            if let Some(event) = self.pending_events.pop_front() {
                return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(event));
            }
        }

        // Process pending commands
        while let Some(cmd) = self.pending_commands.pop_front() {
            match cmd {
                HandlerCommand::Activate { overlay, node_type } => {
                    self.activate(overlay, node_type);
                    if let Some(event) = self.pending_events.pop_front() {
                        return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(event));
                    }
                }
                HandlerCommand::AnnouncePricing { threshold } => {
                    if !self.pricing_sent && !self.pricing_outbound_pending {
                        self.pricing_outbound_pending = true;
                        let announce =
                            vertex_swarm_net_pricing::AnnouncePaymentThreshold::new(threshold);
                        let upgrade = ClientOutboundUpgrade::pricing(announce);
                        return Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
                            protocol: SubstreamProtocol::new(upgrade, ClientOutboundInfo::Pricing)
                                .with_timeout(self.config.timeout),
                        });
                    }
                }
                HandlerCommand::RetrieveChunk { address, response } => {
                    let upgrade = ClientOutboundUpgrade::retrieval(address);
                    return Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
                        protocol: SubstreamProtocol::new(
                            upgrade,
                            ClientOutboundInfo::Retrieval {
                                address,
                                response,
                                requested_at: Instant::now(),
                            },
                        )
                        .with_timeout(self.config.retrieval_timeout),
                    });
                }
                HandlerCommand::PushChunk { chunk, response } => {
                    let address = *chunk.address();
                    let delivery = vertex_swarm_net_pushsync::Delivery::new(chunk);
                    let upgrade = ClientOutboundUpgrade::pushsync(delivery);
                    return Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
                        protocol: SubstreamProtocol::new(
                            upgrade,
                            ClientOutboundInfo::Pushsync {
                                address,
                                response,
                                requested_at: Instant::now(),
                            },
                        )
                        .with_timeout(self.config.pushsync_timeout),
                    });
                }
                HandlerCommand::SendPseudosettle { amount } => {
                    let payment = vertex_swarm_net_pseudosettle::Payment::new(amount);
                    let upgrade = ClientOutboundUpgrade::pseudosettle(payment);
                    return Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
                        protocol: SubstreamProtocol::new(
                            upgrade,
                            ClientOutboundInfo::Pseudosettle { amount },
                        )
                        .with_timeout(self.config.timeout),
                    });
                }
                #[cfg(feature = "swap")]
                HandlerCommand::SendCheque { cheque } => {
                    let upgrade =
                        ClientOutboundUpgrade::swap(cheque, self.config.swap_exchange_rate);
                    return Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
                        protocol: SubstreamProtocol::new(upgrade, ClientOutboundInfo::Swap)
                            .with_timeout(self.config.timeout),
                    });
                }
                HandlerCommand::AckPseudosettle { request_id, ack } => {
                    if let Some(result) = self.take_response(request_id) {
                        debug!(%request_id, amount = %ack.amount, "Sending pseudosettle ack");
                        if self
                            .response_sends
                            .try_push(async move {
                                result
                                    .respond(ack)
                                    .await
                                    .map_err(|e| format!("pseudosettle ack: {e}"))
                            })
                            .is_err()
                        {
                            warn!("Response send queue full, dropping pseudosettle ack");
                        }
                    } else {
                        warn!(%request_id, "No pseudosettle responder found for request_id");
                    }
                }
            }
        }

        Poll::Pending
    }

    fn on_behaviour_event(&mut self, event: Self::FromBehaviour) {
        if self.pending_commands.len() >= self.config.max_pending_commands {
            warn!("Handler command queue full, dropping command");
            metrics::counter!("swarm.client.handler.commands_dropped").increment(1);
            return;
        }
        self.pending_commands.push_back(event);
    }

    fn on_connection_event(
        &mut self,
        event: ConnectionEvent<
            Self::InboundProtocol,
            Self::OutboundProtocol,
            Self::InboundOpenInfo,
            Self::OutboundOpenInfo,
        >,
    ) {
        match event {
            ConnectionEvent::FullyNegotiatedInbound(FullyNegotiatedInbound {
                protocol: output,
                ..
            }) => {
                self.handle_inbound_output(output);
            }

            ConnectionEvent::FullyNegotiatedOutbound(FullyNegotiatedOutbound {
                protocol: output,
                info,
                ..
            }) => {
                self.handle_outbound_output(output, info);
            }

            ConnectionEvent::DialUpgradeError(e) => {
                // Classify from the typed upgrade error while it is still
                // concrete: a malformed chunk delivered on the outbound
                // substream fails reconstruction at decode and arrives here as
                // an `Apply` error we can downcast, not a string we parse.
                let apply_error = match &e.error {
                    libp2p::swarm::StreamUpgradeError::Apply(err) => Some(err),
                    _ => None,
                };
                // The per-protocol upgrade deadline fired: the substream
                // negotiated but the response frame never arrived within
                // `retrieval_timeout`/`pushsync_timeout`. This is the liveness
                // boundary against a withholding peer. The chunk-transfer arms
                // resolve the caller with the typed `ChunkTransferError::TimedOut`
                // rather than flattening it into a `Protocol(String)`, but still
                // emit `FailureKind::Protocol` for scoring so the existing
                // retrieval/push failure path (-2.0) is unchanged.
                let timed_out = matches!(&e.error, libp2p::swarm::StreamUpgradeError::Timeout);
                let error = e.error.to_string();
                match e.info {
                    ClientOutboundInfo::Pricing => {
                        self.pricing_outbound_pending = false;
                        warn!(protocol = "pricing", %error, "Client dial upgrade error");
                        self.push_event(HandlerEvent::Error {
                            overlay: self.overlay(),
                            protocol: "pricing",
                            error,
                        });
                    }
                    ClientOutboundInfo::Retrieval {
                        address,
                        response,
                        requested_at,
                    } => {
                        // A timeout is never a malformed chunk, so it scores as a
                        // plain protocol failure; an `Apply` error may be a
                        // malformed delivery and is classified as before.
                        let kind = apply_error
                            .map_or(FailureKind::Protocol, |e| e.retrieval_failure_kind());
                        if timed_out {
                            // Sole emission site for the retrieval timeout
                            // counter: the client handler resolves the response
                            // here and never routes a chunk-transfer upgrade error
                            // through the shared headers `UpgradeError::record`
                            // path, so there is no double count.
                            metrics::counter!("swarm.client.retrieval_timeouts_total").increment(1);
                            debug!(
                                peer_overlay = ?self.overlay(),
                                %address,
                                elapsed = ?requested_at.elapsed(),
                                "Retrieval timed out waiting on a withholding peer"
                            );
                        }
                        warn!(protocol = "retrieval", %address, %error, ?kind, "Client dial upgrade error");
                        if let Some(overlay) = self.overlay() {
                            self.push_event(HandlerEvent::RetrievalFailed {
                                overlay,
                                address,
                                error: error.clone(),
                                kind,
                            });
                        }
                        let outcome = if timed_out {
                            ChunkTransferError::TimedOut
                        } else {
                            ChunkTransferError::Protocol(error)
                        };
                        let _ = response.send(Err(outcome));
                    }
                    ClientOutboundInfo::Pushsync {
                        address,
                        response,
                        requested_at,
                    } => {
                        let kind = apply_error
                            .map_or(FailureKind::Protocol, |e| e.pushsync_failure_kind());
                        if timed_out {
                            // Sole emission site for the pushsync timeout counter,
                            // mirroring the retrieval arm above.
                            metrics::counter!("swarm.client.pushsync_timeouts_total").increment(1);
                            debug!(
                                peer_overlay = ?self.overlay(),
                                %address,
                                elapsed = ?requested_at.elapsed(),
                                "Pushsync timed out waiting on a withholding peer"
                            );
                        }
                        warn!(protocol = "pushsync", %address, %error, ?kind, "Client dial upgrade error");
                        if let Some(overlay) = self.overlay() {
                            self.push_event(HandlerEvent::PushFailed {
                                overlay,
                                address,
                                error: error.clone(),
                                kind,
                            });
                        }
                        let outcome = if timed_out {
                            ChunkTransferError::TimedOut
                        } else {
                            ChunkTransferError::Protocol(error)
                        };
                        let _ = response.send(Err(outcome));
                    }
                    ClientOutboundInfo::Pseudosettle { .. } => {
                        warn!(protocol = "pseudosettle", %error, "Client dial upgrade error");
                        self.push_event(HandlerEvent::Error {
                            overlay: self.overlay(),
                            protocol: "pseudosettle",
                            error,
                        });
                    }
                    #[cfg(feature = "swap")]
                    ClientOutboundInfo::Swap => {
                        warn!(protocol = "swap", %error, "Client dial upgrade error");
                        self.push_event(HandlerEvent::Error {
                            overlay: self.overlay(),
                            protocol: "swap",
                            error,
                        });
                    }
                }
            }

            ConnectionEvent::ListenUpgradeError(e) => {
                // A peer pushing us a malformed chunk, or sending a malformed
                // retrieval request, fails reconstruction at inbound decode and
                // surfaces here. Classify from the typed error so the offending
                // peer is scored adversely; the chunk is already rejected and
                // never relayed.
                let kind = e.error.inbound_failure_kind();
                warn!(error = %e.error, ?kind, "Client listen upgrade error");
                match (kind, self.overlay()) {
                    (FailureKind::InvalidChunk, Some(overlay)) => {
                        let protocol = match &e.error {
                            ClientUpgradeError::Pushsync(_) => PUSHSYNC_PROTOCOL,
                            ClientUpgradeError::Retrieval(_) => RETRIEVAL_PROTOCOL,
                            _ => "unknown",
                        };
                        self.push_event(HandlerEvent::InboundInvalidData { overlay, protocol });
                    }
                    _ => {
                        self.push_event(HandlerEvent::Error {
                            overlay: self.overlay(),
                            protocol: "unknown",
                            error: e.error.to_string(),
                        });
                    }
                }
            }

            _ => {}
        }
    }
}

impl ClientHandler {
    /// Handle an inbound protocol completion.
    fn handle_inbound_output(&mut self, output: ClientInboundOutput) {
        match output {
            ClientInboundOutput::Pricing(threshold) => {
                self.on_pricing_received(threshold);
            }
            ClientInboundOutput::Retrieval(request, responder) => {
                self.on_retrieval_request(request, responder);
            }
            ClientInboundOutput::Pushsync(delivery, responder) => {
                self.on_pushsync_delivery(delivery, responder);
            }
            ClientInboundOutput::Pseudosettle(result) => {
                if let Some(overlay) = self.overlay() {
                    let request_id = self.next_request_id();
                    debug!(%overlay, amount = %result.payment.amount, %request_id, "Received pseudosettle payment");
                    self.pending_events
                        .push_back(HandlerEvent::PseudosettleReceived {
                            overlay,
                            amount: result.payment.amount,
                            request_id,
                        });
                    self.store_response(request_id, result);
                }
            }
            #[cfg(feature = "swap")]
            ClientInboundOutput::Swap(cheque, headers) => {
                if let Some(overlay) = self.overlay() {
                    debug!(%overlay, peer_rate = %headers.exchange_rate, "Received swap cheque");
                    self.push_event(HandlerEvent::SwapChequeReceived {
                        overlay,
                        cheque,
                        peer_rate: headers.exchange_rate,
                    });
                }
            }
        }
    }

    /// Handle an outbound protocol completion.
    fn handle_outbound_output(&mut self, output: ClientOutboundOutput, info: ClientOutboundInfo) {
        match (output, info) {
            (ClientOutboundOutput::Pricing, ClientOutboundInfo::Pricing) => {
                self.pricing_sent = true;
                self.pricing_outbound_pending = false;
                if let Some(overlay) = self.overlay() {
                    self.pending_events
                        .push_back(HandlerEvent::PricingSent { overlay });
                }
            }
            (
                ClientOutboundOutput::Retrieval(delivery),
                ClientOutboundInfo::Retrieval {
                    address,
                    response,
                    requested_at,
                },
            ) => {
                let latency = requested_at.elapsed();
                self.on_retrieval_response(delivery, address, response, latency);
            }
            (
                ClientOutboundOutput::Pushsync(receipt),
                ClientOutboundInfo::Pushsync {
                    address,
                    response,
                    requested_at,
                },
            ) => {
                let latency = requested_at.elapsed();
                debug!(%address, "Received pushsync receipt");
                self.on_pushsync_receipt(receipt, address, response, latency);
            }
            (
                ClientOutboundOutput::Pseudosettle(ack),
                ClientOutboundInfo::Pseudosettle { amount },
            ) => {
                if let Some(overlay) = self.overlay() {
                    if ack.amount != amount {
                        warn!(
                            %overlay,
                            sent = %amount,
                            acked = %ack.amount,
                            "Pseudosettle ack amount mismatch"
                        );
                    }
                    debug!(%overlay, %amount, ack_amount = %ack.amount, "Pseudosettle sent");
                    self.pending_events
                        .push_back(HandlerEvent::PseudosettleSent { overlay, ack });
                }
            }
            #[cfg(feature = "swap")]
            (ClientOutboundOutput::Swap(headers), ClientOutboundInfo::Swap) => {
                if let Some(overlay) = self.overlay() {
                    debug!(%overlay, peer_rate = %headers.exchange_rate, "Swap cheque sent");
                    self.push_event(HandlerEvent::SwapChequeSent {
                        overlay,
                        peer_rate: headers.exchange_rate,
                    });
                }
            }
            (output, info) => {
                warn!(?output, ?info, "Mismatched outbound output and info");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{B256, Signature};
    use nectar_postage::Stamp;
    use nectar_primitives::{AnyChunk, ContentChunk};
    use vertex_swarm_primitives::{StampedChunk, StampedChunkExt};

    fn stamped(payload: &'static [u8]) -> StampedChunk {
        let sig = Signature::from_raw(&[1u8; 65]).expect("valid signature");
        let stamp = Stamp::new(B256::repeat_byte(0xaa), 3, 7, 42, sig);
        let chunk: AnyChunk = ContentChunk::new(payload)
            .expect("valid content chunk")
            .into();
        StampedChunk::new(chunk, stamp)
    }

    #[test]
    fn verify_answers_gate_accepts_matching_chunk() {
        let chunk = stamped(b"serve gate payload");
        let requested = *chunk.address();
        assert!(chunk.verify_answers(requested).is_ok());
    }

    #[test]
    fn verify_answers_gate_rejects_mismatched_chunk() {
        let chunk = stamped(b"actual payload");
        let other = stamped(b"a different payload entirely");
        let requested = *other.address();
        assert_ne!(*chunk.address(), requested);
        assert!(chunk.verify_answers(requested).is_err());
    }
}
