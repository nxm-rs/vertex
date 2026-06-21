//! Per-connection handler for pullsync.
//!
//! Inbound is the syncer: cursors and range substreams are served by
//! self-contained futures that read [`PullStorage`] and send the reply inside
//! the future, resolving to an outcome the handler turns into a metric. Outbound
//! is the puller command surface: a `FetchCursors` resolves in the upgrade, a
//! `SyncRange` is driven to completion by an outbound future that selects the
//! whole offer and collects the deliveries.

use std::{
    collections::VecDeque,
    num::NonZeroU32,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use futures::{
    future::BoxFuture,
    stream::{FuturesUnordered, StreamExt},
};
use libp2p::{
    PeerId,
    swarm::{
        SubstreamProtocol,
        handler::{
            ConnectionEvent, ConnectionHandler, ConnectionHandlerEvent, DialUpgradeError,
            FullyNegotiatedInbound, FullyNegotiatedOutbound, ListenUpgradeError,
        },
    },
};
use tracing::{debug, warn};
use vertex_net_ratelimiter::{KeyedRateLimiter, Quota};
use vertex_swarm_api::{Bin, ChunkAddress, PullStorage, StampedChunk, SwarmResult};
use vertex_swarm_net_handler_core::HandlerCore;
use vertex_swarm_net_pullsync::{
    Ack, BitVector, ChunkDescriptor, DEFAULT_MAX_PAGE, Delivery, Get, Offer, SyncRequester,
    SyncResponder, Want,
};
use vertex_swarm_primitives::all_bins;
use vertex_tasks::time::timeout;

use crate::error::PullsyncFailure;
use crate::upgrade::{
    InboundOutput, OutboundOutput, PullsyncInboundUpgrade, PullsyncOutboundUpgrade,
};

/// Per-connection inbound substream-open quota: headroom for a burst of cursor
/// and range opens while throttling a peer that loops new streams.
const INBOUND_SUBSTREAM_QUOTA: Quota = Quota::n_every(nonzero(8), Duration::from_secs(20));

/// Deadline on the headers exchange and first framed read of an inbound stream.
const STREAM_TIMEOUT: Duration = Duration::from_secs(10);

/// Outbound deadline covering the upgrade and the offer read.
const OUTBOUND_TIMEOUT: Duration = Duration::from_secs(30);

/// Deadline on each post-upgrade framed read. The upgrade timeout bounds
/// negotiation and the first read only; without this a peer that opens a sync
/// stream and then stalls (takes our offer but never sends a want, or withholds
/// deliveries) would pin a serving or driving slot indefinitely.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Cap on concurrent inbound serving futures per connection. Once full,
/// `listen_protocol` stops advertising serving so the muxer back-pressures.
const MAX_INBOUND_SERVING: usize = 16;

/// Cap on concurrent outbound range futures per connection.
const MAX_OUTBOUND_DRIVING: usize = 8;

/// Cap on commands queued from the behaviour before the oldest is dropped.
const MAX_PENDING_COMMANDS: usize = 64;

const fn nonzero(n: u32) -> NonZeroU32 {
    match NonZeroU32::new(n) {
        Some(v) => v,
        None => unreachable!(),
    }
}

/// Commands from the behaviour to the handler.
#[derive(Debug)]
pub enum PullsyncCommand {
    /// Open the cursor handshake and report the peer's per-bin cursors.
    FetchCursors,
    /// Open a range exchange for `bin` from `start` and collect the deliveries.
    SyncRange { bin: Bin, start: u64 },
}

/// Events from the handler to the behaviour.
#[derive(Debug)]
pub enum PullsyncHandlerEvent {
    /// Cursor handshake answered.
    CursorsReceived { cursors: Vec<u64>, epoch: u64 },
    /// Range exchange delivered chunks for `bin`, with `topmost` the highest id
    /// the offer covered (the puller advances its cursor to it).
    RangeDelivered {
        bin: Bin,
        topmost: u64,
        chunks: Vec<StampedChunk>,
    },
    /// An outbound command failed.
    OutboundFailed { failure: PullsyncFailure },
}

/// Outcome of one inbound serving future. The reply is already sent inside the
/// future; this carries only the metrics signal.
enum InboundOutcome {
    Cursors,
    Range { delivered: u64 },
    Failed,
}

/// Outcome of one outbound range future.
enum RangeOutcome {
    Delivered {
        bin: Bin,
        topmost: u64,
        chunks: Vec<StampedChunk>,
    },
    Failed(PullsyncFailure),
}

/// Per-connection pullsync handler.
pub struct PullsyncHandler {
    remote_peer_id: PeerId,
    /// Server snapshot the inbound responders read.
    storage: Arc<dyn PullStorage>,
    /// Shared core: pending events and the inbound substream-open limiter.
    core: HandlerCore<PullsyncHandlerEvent>,
    /// Shared with the behaviour so the per-peer chunks-per-second bucket
    /// survives reconnects; freed on the final `ConnectionClosed`.
    chunk_limit: Arc<KeyedRateLimiter<PeerId>>,
    pending_commands: VecDeque<PullsyncCommand>,
    inbound: FuturesUnordered<BoxFuture<'static, InboundOutcome>>,
    outbound: FuturesUnordered<BoxFuture<'static, RangeOutcome>>,
}

impl PullsyncHandler {
    pub fn new(
        remote_peer_id: PeerId,
        storage: Arc<dyn PullStorage>,
        chunk_limit: Arc<KeyedRateLimiter<PeerId>>,
    ) -> Self {
        Self {
            remote_peer_id,
            storage,
            core: HandlerCore::new(INBOUND_SUBSTREAM_QUOTA),
            chunk_limit,
            pending_commands: VecDeque::new(),
            inbound: FuturesUnordered::new(),
            outbound: FuturesUnordered::new(),
        }
    }

    /// Answer a cursor handshake from the local per-bin cursors and reserve epoch.
    fn serve_cursors(&mut self, responder: vertex_swarm_net_pullsync::CursorsResponder) {
        let storage = Arc::clone(&self.storage);
        self.inbound.push(Box::pin(async move {
            // A storage fault must surface as a failed serve; advertising 0
            // cursors would mask the fault and stall the peer's sync.
            let mut cursors = Vec::with_capacity(Bin::COUNT);
            for bin in all_bins(Bin::MAX) {
                match storage.bin_cursor(bin) {
                    Ok(cursor) => cursors.push(cursor),
                    Err(e) => {
                        warn!(error = %e, %bin, "Pullsync bin cursor read failed");
                        return InboundOutcome::Failed;
                    }
                }
            }
            let ack = Ack {
                cursors,
                epoch: storage.reserve_epoch(),
            };
            match responder.send_ack(ack).await {
                Ok(()) => InboundOutcome::Cursors,
                Err(e) => {
                    debug!(error = %e, "Pullsync cursors serve failed");
                    InboundOutcome::Failed
                }
            }
        }));
    }

    /// Serve a range request: page the bin, offer descriptors, deliver the wanted
    /// chunks under the per-second cap.
    fn serve_range(&mut self, get: Get, responder: SyncResponder) {
        let storage = Arc::clone(&self.storage);
        let chunk_limit = Arc::clone(&self.chunk_limit);
        let peer = self.remote_peer_id;
        self.inbound.push(Box::pin(serve_range_inner(
            storage,
            chunk_limit,
            peer,
            get,
            responder,
        )));
    }

    /// Drive a negotiated outbound range to completion.
    fn drive_range(&mut self, bin: Bin, offer: Offer, requester: SyncRequester) {
        self.outbound
            .push(Box::pin(drive_range_inner(bin, offer, requester)));
    }

    /// Index of the first queued command that may dispatch now. A `FetchCursors`
    /// always may; a `SyncRange` only while the outbound driving cap has room.
    fn next_dispatchable_command(&self) -> Option<usize> {
        let has_range_slot = self.outbound.len() < MAX_OUTBOUND_DRIVING;
        self.pending_commands.iter().position(|cmd| match cmd {
            PullsyncCommand::FetchCursors => true,
            PullsyncCommand::SyncRange { .. } => has_range_slot,
        })
    }
}

/// Page the requested bin into an offer, read the want, and stream the selected
/// chunks. The per-peer chunks-per-second bucket trims the selection so a
/// requester cannot drain us faster than the reserve rate cap.
async fn serve_range_inner(
    storage: Arc<dyn PullStorage>,
    chunk_limit: Arc<KeyedRateLimiter<PeerId>>,
    peer: PeerId,
    get: Get,
    responder: SyncResponder,
) -> InboundOutcome {
    let (descriptors, topmost) = match page_bin(&storage, get.bin, get.start) {
        Ok(page) => page,
        Err(e) => {
            debug!(error = %e, "Pullsync range scan failed");
            return InboundOutcome::Failed;
        }
    };

    let offer = Offer::new(topmost, descriptors.iter().map(|(_, d)| *d).collect());
    let responder = match responder.write_offer(offer).await {
        Ok(r) => r,
        Err(e) => {
            debug!(error = %e, "Pullsync offer send failed");
            return InboundOutcome::Failed;
        }
    };

    // An empty page ends the exchange after the offer, with no want round.
    if descriptors.is_empty() {
        return match responder.finish().await {
            Ok(()) => InboundOutcome::Range { delivered: 0 },
            Err(e) => {
                debug!(error = %e, "Pullsync empty offer finish failed");
                InboundOutcome::Failed
            }
        };
    }

    // Bound the want read: a peer that takes the offer and never answers must not
    // hold the serving slot past the deadline.
    let (want, mut responder) = match timeout(READ_TIMEOUT, responder.read_want()).await {
        Ok(Ok(pair)) => pair,
        Ok(Err(e)) => {
            debug!(error = %e, "Pullsync want read failed");
            return InboundOutcome::Failed;
        }
        Err(_) => {
            debug!(%peer, "Pullsync want read timed out");
            return InboundOutcome::Failed;
        }
    };

    let mut delivered = 0u64;
    for (i, (address, _)) in descriptors.iter().enumerate() {
        if !want.wanted.get(i) {
            continue;
        }
        // One token per chunk; stop serving this page once the bucket is empty
        // rather than blocking.
        if chunk_limit.try_consume(peer).is_err() {
            debug!(%peer, "Pullsync chunk rate limit reached; truncating page");
            break;
        }
        let cached = match storage.get(address) {
            Ok(Some(cached)) => cached,
            // Evicted between the offer and this read: skip it, sending fewer
            // deliveries than wanted bits. A puller matches deliveries by address
            // and reads to stream close, so the shortfall is tolerated; this is a
            // deliberate divergence from sending a placeholder for the gap.
            Ok(None) => continue,
            Err(e) => {
                debug!(error = %e, "Pullsync chunk fetch failed");
                return InboundOutcome::Failed;
            }
        };
        let (chunk, stamp) = cached.into_parts();
        // A reserve entry is always stamped; a stampless one is not a valid
        // delivery, so skip it rather than send a half chunk.
        let Some(stamp) = stamp else { continue };
        if let Err(e) = responder
            .send_delivery(Delivery::new(StampedChunk::new(chunk, stamp)))
            .await
        {
            debug!(error = %e, "Pullsync delivery send failed");
            return InboundOutcome::Failed;
        }
        delivered += 1;
    }

    match responder.finish().await {
        Ok(()) => InboundOutcome::Range { delivered },
        Err(e) => {
            debug!(error = %e, "Pullsync delivery finish failed");
            InboundOutcome::Failed
        }
    }
}

/// Collect up to [`DEFAULT_MAX_PAGE`] descriptors for `bin` from `start`, paired
/// with their address, plus the topmost sequence covered.
#[allow(clippy::type_complexity)]
fn page_bin(
    storage: &Arc<dyn PullStorage>,
    bin: Bin,
    start: u64,
) -> SwarmResult<(Vec<(ChunkAddress, ChunkDescriptor)>, u64)> {
    let mut descriptors = Vec::new();
    // Raised only from scanned ids; an empty range yields topmost 0, never
    // `start`. The puller advances its cursor to `max(topmost, start)`, so
    // returning `start` here would falsely mark an empty range as synced.
    let mut topmost = 0u64;
    for item in storage.scan_bin_from(bin, start)? {
        let item = item?;
        topmost = topmost.max(item.seq);
        descriptors.push((
            item.address,
            ChunkDescriptor::new(item.address, item.batch_id, item.stamp_hash),
        ));
        if descriptors.len() as u64 >= DEFAULT_MAX_PAGE {
            break;
        }
    }
    Ok((descriptors, topmost))
}

/// Select every chunk in the offer, send the want, and collect the deliveries.
/// Admission and verification belong to the puller service; the command surface
/// wants all offered chunks so the round-trip is exercised end to end.
async fn drive_range_inner(bin: Bin, offer: Offer, requester: SyncRequester) -> RangeOutcome {
    let topmost = offer.topmost;
    let expected = offer.chunks.len();

    // An empty offer ends the exchange with no want; the topmost still advances
    // the puller's cursor past the now-known-empty range.
    if expected == 0 {
        return match requester.finish().await {
            Ok(()) => RangeOutcome::Delivered {
                bin,
                topmost,
                chunks: Vec::new(),
            },
            Err(e) => RangeOutcome::Failed(PullsyncFailure::Stream(e.to_string())),
        };
    }

    let mut want = BitVector::new(expected);
    for i in 0..expected {
        want.set(i);
    }
    let mut requester = match requester.send_want(Want::new(want)).await {
        Ok(r) => r,
        Err(e) => return RangeOutcome::Failed(PullsyncFailure::Stream(e.to_string())),
    };

    let mut chunks = Vec::with_capacity(expected);
    loop {
        // Bound each delivery read: a responder that takes our want and then
        // withholds deliveries must not pin the driving slot past the deadline.
        match timeout(READ_TIMEOUT, requester.next_delivery()).await {
            Ok(Ok(Some(delivery))) => chunks.push(*delivery.chunk),
            Ok(Ok(None)) => break,
            Ok(Err(e)) => return RangeOutcome::Failed(PullsyncFailure::Stream(e.to_string())),
            Err(_) => return RangeOutcome::Failed(PullsyncFailure::TimedOut),
        }
    }
    RangeOutcome::Delivered {
        bin,
        topmost,
        chunks,
    }
}

impl ConnectionHandler for PullsyncHandler {
    type FromBehaviour = PullsyncCommand;
    type ToBehaviour = PullsyncHandlerEvent;
    type InboundProtocol = PullsyncInboundUpgrade;
    type OutboundProtocol = PullsyncOutboundUpgrade;
    type InboundOpenInfo = ();
    type OutboundOpenInfo = OutboundInfo;

    fn listen_protocol(&self) -> SubstreamProtocol<Self::InboundProtocol, Self::InboundOpenInfo> {
        SubstreamProtocol::new(PullsyncInboundUpgrade, ()).with_timeout(STREAM_TIMEOUT)
    }

    fn connection_keep_alive(&self) -> bool {
        true
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<
        ConnectionHandlerEvent<Self::OutboundProtocol, Self::OutboundOpenInfo, Self::ToBehaviour>,
    > {
        if let Some(event) = self.core.poll_pending() {
            return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(event));
        }

        while let Poll::Ready(Some(outcome)) = self.inbound.poll_next_unpin(cx) {
            match outcome {
                InboundOutcome::Cursors => crate::metrics::inbound_cursors_served(),
                InboundOutcome::Range { delivered } => {
                    crate::metrics::inbound_range_served(delivered)
                }
                InboundOutcome::Failed => crate::metrics::inbound_failed(),
            }
        }

        if let Poll::Ready(Some(outcome)) = self.outbound.poll_next_unpin(cx) {
            let event = match outcome {
                RangeOutcome::Delivered {
                    bin,
                    topmost,
                    chunks,
                } => {
                    crate::metrics::outbound_range_delivered(chunks.len() as u64);
                    PullsyncHandlerEvent::RangeDelivered {
                        bin,
                        topmost,
                        chunks,
                    }
                }
                RangeOutcome::Failed(failure) => {
                    crate::metrics::outbound_failed();
                    PullsyncHandlerEvent::OutboundFailed { failure }
                }
            };
            return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(event));
        }

        // A `FetchCursors` resolves entirely in the upgrade and occupies no
        // outbound driving slot, so only `SyncRange` is gated on the cap. Gating
        // both would head-of-line block cursor fetches behind saturated range
        // drives.
        if let Some(idx) = self.next_dispatchable_command()
            && let Some(cmd) = self.pending_commands.remove(idx)
        {
            let (protocol, info) = match cmd {
                PullsyncCommand::FetchCursors => {
                    (PullsyncOutboundUpgrade::Cursors, OutboundInfo::Cursors)
                }
                PullsyncCommand::SyncRange { bin, start } => (
                    PullsyncOutboundUpgrade::Sync(Get::new(bin, start)),
                    OutboundInfo::Sync { bin },
                ),
            };
            return Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
                protocol: SubstreamProtocol::new(protocol, info).with_timeout(OUTBOUND_TIMEOUT),
            });
        }

        Poll::Pending
    }

    fn on_behaviour_event(&mut self, event: Self::FromBehaviour) {
        if self.pending_commands.len() >= MAX_PENDING_COMMANDS {
            warn!(peer_id = %self.remote_peer_id, "Pullsync command queue full, dropping oldest");
            self.pending_commands.pop_front();
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
                protocol, ..
            }) => {
                if !self.core.try_accept_inbound() || self.inbound.len() >= MAX_INBOUND_SERVING {
                    warn!(peer_id = %self.remote_peer_id, "Rate limiting inbound pullsync stream");
                    crate::metrics::inbound_rate_limited();
                    return;
                }
                match protocol {
                    InboundOutput::Cursors(responder) => self.serve_cursors(responder),
                    InboundOutput::Sync(get, responder) => self.serve_range(get, responder),
                }
            }

            ConnectionEvent::FullyNegotiatedOutbound(FullyNegotiatedOutbound {
                protocol,
                info,
            }) => match (protocol, info) {
                (OutboundOutput::Cursors(ack), OutboundInfo::Cursors) => {
                    crate::metrics::outbound_cursors_received();
                    self.core.push_event(PullsyncHandlerEvent::CursorsReceived {
                        cursors: ack.cursors,
                        epoch: ack.epoch,
                    });
                }
                (OutboundOutput::Sync(offer, requester), OutboundInfo::Sync { bin }) => {
                    self.drive_range(bin, offer, requester);
                }
                (_, info) => warn!(?info, "Mismatched pullsync outbound output"),
            },

            ConnectionEvent::DialUpgradeError(DialUpgradeError { error, info }) => {
                let failure = classify(&error.to_string());
                debug!(?info, %error, "Pullsync outbound error");
                self.core
                    .push_event(PullsyncHandlerEvent::OutboundFailed { failure });
            }

            ConnectionEvent::ListenUpgradeError(ListenUpgradeError { error, .. }) => {
                debug!(%error, "Pullsync inbound error");
                crate::metrics::inbound_failed();
            }

            _ => {}
        }
    }
}

/// Map a stream-upgrade error to the typed failure, preserving the timeout
/// signal so the puller service can tell a stall from a fault.
fn classify(error: &str) -> PullsyncFailure {
    if error.contains("Timeout") || error.contains("timeout") {
        PullsyncFailure::TimedOut
    } else {
        PullsyncFailure::Stream(error.to_string())
    }
}

/// Per-connection outbound open info carried alongside an outbound request.
#[derive(Debug)]
pub enum OutboundInfo {
    Cursors,
    Sync { bin: Bin },
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::future::pending;

    // A stalled post-upgrade read elapses at `READ_TIMEOUT` instead of pending
    // forever, the property both the inbound want read and the outbound delivery
    // read rely on to drop the exchange and free its slot. Mirrors the failure
    // mapping the outbound drive uses on a withholding responder.
    #[tokio::test(start_paused = true)]
    async fn stalled_read_times_out_and_maps_to_failure() {
        let stalled = pending::<Result<(), PullsyncFailure>>();
        let outcome = match timeout(READ_TIMEOUT, stalled).await {
            Ok(Ok(())) => RangeOutcome::Delivered {
                bin: Bin::ZERO,
                topmost: 0,
                chunks: Vec::new(),
            },
            Ok(Err(e)) => RangeOutcome::Failed(PullsyncFailure::Stream(e.to_string())),
            Err(_) => RangeOutcome::Failed(PullsyncFailure::TimedOut),
        };
        assert!(matches!(
            outcome,
            RangeOutcome::Failed(PullsyncFailure::TimedOut)
        ));
    }
}
