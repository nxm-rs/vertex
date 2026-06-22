//! The [`Puller`]: readiness-gated background driver that pull-syncs the
//! neighbourhood into the reserve.
//!
//! Per pass it awaits neighbourhood readiness, then for each neighbour fetches
//! cursors, resets that peer's intervals on a reserve-epoch change, and for each
//! in-scope bin drives `sync_range` from the persisted interval upward,
//! verifying and admitting each delivered chunk before advancing the interval.
//! When caught up it backs off and re-passes (live tail).

use std::collections::HashSet;
use std::time::Duration;

use libp2p::PeerId;
use tokio::sync::mpsc;
use tracing::{debug, warn};
use vertex_swarm_api::{
    IntervalStore, PeerReporter, PullChunkVerifier, ReportSource, SwarmScoringEvent,
};
use vertex_swarm_primitives::{Bin, OverlayAddress};
use vertex_tasks::{GracefulShutdown, MaybeSend, SpawnableTask, time};

use crate::seams::{
    NeighbourSource, PullsyncControl, PullsyncEvent, ReadinessGate, ReserveAdmit, SyncTarget,
};

/// Reporting source stamped on puller-originated scoring events.
const PULLSYNC_SOURCE: ReportSource = ReportSource::Protocol("pullsync");

/// Backoff between sync passes once the neighbourhood is caught up.
pub const DEFAULT_TAIL_BACKOFF: Duration = Duration::from_secs(5);

/// Ceiling on awaiting a single peer's cursor or range reply, above the wire
/// handler's own outbound and read bounds so a slow exchange is not cut off.
pub const DEFAULT_PEER_RESPONSE_TIMEOUT: Duration = Duration::from_secs(45);

/// Tuning for the [`Puller`] loop.
#[derive(Debug, Clone, Copy)]
pub struct PullerConfig {
    /// Backoff applied after a caught-up pass before tailing again.
    pub tail_backoff: Duration,
    /// Ceiling on awaiting one peer's reply before abandoning that target; a
    /// never-connected or silent peer yields no `Failed` event, so without this
    /// the per-peer await blocks the whole pass forever.
    pub peer_response_timeout: Duration,
}

impl Default for PullerConfig {
    fn default() -> Self {
        Self {
            tail_backoff: DEFAULT_TAIL_BACKOFF,
            peer_response_timeout: DEFAULT_PEER_RESPONSE_TIMEOUT,
        }
    }
}

/// The decoupling seams the [`Puller`] drives, bundled so the loop and its
/// constructors take one value rather than seven.
pub struct PullerSeams<C, S, V, A, G, N, R> {
    /// Outbound command surface, bridged to `PullsyncBehaviour`.
    pub control: C,
    /// Per-peer interval and epoch persistence.
    pub intervals: S,
    /// Admission gate run before a chunk enters the reserve.
    pub verifier: V,
    /// Reserve put seam.
    pub admit: A,
    /// Neighbourhood readiness gate.
    pub readiness: G,
    /// Source of neighbours and in-scope bins.
    pub neighbours: N,
    /// Scoring sink for peers that serve unverifiable chunks.
    pub reporter: R,
}

/// Neighbourhood pull-sync service.
///
/// Generic over the decoupling seams so it runs against the live node or test
/// mocks unchanged. The reserve epoch is read from each peer's cursor handshake;
/// a change resets that peer's persisted intervals so a wiped or recreated source
/// reserve is re-synced from zero.
pub struct Puller<C, E, S, V, A, G, N, R> {
    control: C,
    events: mpsc::Receiver<E>,
    intervals: S,
    verifier: V,
    admit: A,
    readiness: G,
    neighbours: N,
    reporter: R,
    config: PullerConfig,
    /// Monotonic id stamped on each outbound command so a late reply from a
    /// timed-out command cannot be matched to the next command for the same
    /// peer and bin. Local to the in-process surface; never on the wire.
    next_request_id: u64,
}

impl<C, S, V, A, G, N, R> Puller<C, PullsyncEvent, S, V, A, G, N, R>
where
    C: PullsyncControl,
    S: IntervalStore,
    V: PullChunkVerifier,
    A: ReserveAdmit,
    G: ReadinessGate,
    N: NeighbourSource,
    R: PeerReporter,
{
    /// Construct a puller over the decoupling seams and its event receiver.
    pub fn new(
        seams: PullerSeams<C, S, V, A, G, N, R>,
        events: mpsc::Receiver<PullsyncEvent>,
        config: PullerConfig,
    ) -> Self {
        let PullerSeams {
            control,
            intervals,
            verifier,
            admit,
            readiness,
            neighbours,
            reporter,
        } = seams;
        Self {
            control,
            events,
            intervals,
            verifier,
            admit,
            readiness,
            neighbours,
            reporter,
            config,
            next_request_id: 0,
        }
    }

    /// Next outbound command id; wraps after `u64::MAX` commands, which the
    /// await never confuses for a stale in-flight reply.
    fn next_request_id(&mut self) -> u64 {
        let id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1);
        id
    }

    /// Run the sync loop until `shutdown` fires.
    pub async fn run(mut self, shutdown: GracefulShutdown) {
        let mut shutdown = std::pin::pin!(shutdown);

        loop {
            tokio::select! {
                guard = &mut shutdown => {
                    debug!("puller received shutdown signal");
                    drop(guard);
                    break;
                }
                () = self.readiness.wait_ready() => {}
            }

            tokio::select! {
                guard = &mut shutdown => {
                    drop(guard);
                    return;
                }
                () = self.sync_pass() => {}
            }

            tokio::select! {
                guard = &mut shutdown => {
                    drop(guard);
                    break;
                }
                () = time::sleep(self.config.tail_backoff) => {}
            }
        }
        debug!("puller shutdown complete");
    }

    /// One sync pass: fetch and sync every current neighbour target once.
    ///
    /// A peer that serves an unverifiable chunk is reported and added to a
    /// per-pass skip set, so the same poison source is not re-requested for the
    /// remainder of this pass; the next pass re-evaluates targets afresh (by
    /// then scoring may have dropped it).
    ///
    /// Public for deterministic testing; the live driver wraps it with the
    /// readiness gate and tail backoff in [`run`](Self::run).
    pub async fn sync_pass(&mut self) {
        let mut rejected = HashSet::new();
        for target in self.neighbours.targets() {
            if rejected.contains(&target.peer) {
                continue;
            }
            if self.sync_peer(&target).await {
                rejected.insert(target.peer);
            }
        }
    }

    /// Fetch a peer's cursors, reconcile its epoch, then sync each in-scope bin.
    ///
    /// Returns `true` if a delivered chunk failed verification, so the caller
    /// skips this peer for the rest of the pass.
    async fn sync_peer(&mut self, target: &SyncTarget) -> bool {
        let request_id = self.next_request_id();
        self.control.fetch_cursors(target.peer, request_id);

        let epoch = match self.await_cursors(target.peer, request_id).await {
            Some(epoch) => epoch,
            None => return false,
        };

        if let Err(e) = self.reconcile_epoch(&target.overlay, epoch) {
            warn!(overlay = %target.overlay, error = %e, "puller epoch reconcile failed");
            return false;
        }

        for bin in &target.bins {
            if self.sync_bin(target, *bin).await {
                return true;
            }
        }
        false
    }

    /// Drain the event stream until this command's cursor handshake answers,
    /// returning its advertised reserve epoch. Matching is keyed on `request_id`
    /// so a stale reply from a prior timed-out command is discarded rather than
    /// taken for this one. `None` abandons this peer: it failed, the deadline
    /// elapsed, or the stream closed.
    async fn await_cursors(&mut self, peer: PeerId, request_id: u64) -> Option<u64> {
        let ceiling = self.config.peer_response_timeout;
        let events = &mut self.events;
        let drained = async {
            loop {
                match events.recv().await? {
                    PullsyncEvent::CursorsReceived {
                        request_id: id,
                        epoch,
                        ..
                    } if id == request_id => {
                        return Some(epoch);
                    }
                    PullsyncEvent::Failed {
                        request_id: id,
                        failure,
                        ..
                    } if id == request_id => {
                        debug!(%peer, %failure, "puller cursor handshake failed");
                        return None;
                    }
                    _ => continue,
                }
            }
        };
        match time::timeout(ceiling, drained).await {
            Ok(result) => result,
            Err(_elapsed) => {
                warn!(%peer, "puller cursor handshake timed out, abandoning peer");
                None
            }
        }
    }

    /// Reset a peer's persisted intervals when its advertised epoch no longer
    /// matches the last seen value, then record the new epoch.
    fn reconcile_epoch(
        &self,
        overlay: &OverlayAddress,
        epoch: u64,
    ) -> vertex_swarm_api::SwarmResult<()> {
        if self.intervals.peer_epoch(overlay)? == Some(epoch) {
            return Ok(());
        }
        // A new epoch means the source reserve was recreated: every persisted
        // cursor for this peer is stale. The clear and the epoch write must land
        // together, or a crash between them could leave a matching epoch over stale
        // intervals and silently skip data, so reset atomically.
        self.intervals.reset_peer(overlay, epoch)
    }

    /// Drive one bin from its persisted interval upward until caught up.
    ///
    /// Returns `true` only on a peer-blameworthy rejection: that peer is reported
    /// for invalid data and skipped for the rest of the pass. A transient
    /// rejection (an unknown batch during catch-up) also leaves the interval
    /// unadvanced so the page is retried on a later pass, but neither reports nor
    /// skips the peer.
    async fn sync_bin(&mut self, target: &SyncTarget, bin: Bin) -> bool {
        loop {
            let start = match self.intervals.interval(&target.overlay, bin) {
                Ok(start) => start,
                Err(e) => {
                    warn!(overlay = %target.overlay, error = %e, "puller interval read failed");
                    return false;
                }
            };

            let request_id = self.next_request_id();
            self.control.sync_range(target.peer, request_id, bin, start);

            let (topmost, chunks) = match self.await_range(target.peer, request_id, bin).await {
                Some(page) => page,
                None => return false,
            };

            let mut rejected = false;
            let mut blameworthy = false;
            for chunk in chunks {
                match self.verifier.verify(&chunk) {
                    Ok(()) => {
                        if let Err(e) = self.admit.admit(chunk) {
                            warn!(overlay = %target.overlay, error = %e, "puller reserve admit failed");
                        }
                    }
                    Err(e) => {
                        // Any rejection taints the whole offer: do not advance
                        // past it, or the unverified span is skipped forever.
                        rejected = true;
                        blameworthy |= e.is_peer_blameworthy();
                        debug!(overlay = %target.overlay, reason = <&'static str>::from(&e), "puller rejected chunk");
                    }
                }
            }

            // A tainted page never advances the interval. A blameworthy rejection
            // also reports the source for invalid data and skips it for the rest
            // of the pass; a transient one (an unknown batch the indexer has not
            // caught up to) only stops this bin so the page is retried later.
            if rejected {
                if blameworthy {
                    self.reporter.report_peer(
                        &target.overlay,
                        SwarmScoringEvent::InvalidData,
                        PULLSYNC_SOURCE,
                    );
                }
                return blameworthy;
            }

            // Caught up: the offer covered nothing past the resume point.
            if topmost <= start {
                return false;
            }

            if let Err(e) = self.intervals.set_interval(&target.overlay, bin, topmost) {
                warn!(overlay = %target.overlay, error = %e, "puller interval write failed");
                return false;
            }
        }
    }

    /// Drain the event stream until this command's range page answers,
    /// returning `(topmost, chunks)`. Matching is keyed on `request_id` so a
    /// stale reply buffered from a prior timed-out command for the same peer and
    /// bin is discarded rather than advancing the interval past undelivered
    /// data. `None` abandons this peer: it failed, the deadline elapsed, or the
    /// stream closed.
    async fn await_range(
        &mut self,
        peer: PeerId,
        request_id: u64,
        bin: Bin,
    ) -> Option<(u64, Vec<vertex_swarm_primitives::StampedChunk>)> {
        let ceiling = self.config.peer_response_timeout;
        let events = &mut self.events;
        let drained = async {
            loop {
                match events.recv().await? {
                    PullsyncEvent::RangeDelivered {
                        request_id: id,
                        topmost,
                        chunks,
                        ..
                    } if id == request_id => return Some((topmost, chunks)),
                    PullsyncEvent::Failed {
                        request_id: id,
                        failure,
                        ..
                    } if id == request_id => {
                        debug!(%peer, %failure, "puller range exchange failed");
                        return None;
                    }
                    _ => continue,
                }
            }
        };
        match time::timeout(ceiling, drained).await {
            Ok(result) => result,
            Err(_elapsed) => {
                warn!(%peer, %bin, "puller range exchange timed out, abandoning peer");
                None
            }
        }
    }
}

/// Cloneable handle to the puller's event sender, for the node bridge to feed
/// [`PullsyncEvent`]s in.
#[derive(Clone)]
pub struct PullerHandle {
    events: mpsc::Sender<PullsyncEvent>,
}

impl PullerHandle {
    /// Forward a behaviour event into the running puller.
    ///
    /// `Err` carries the rejected event back (channel full or the puller gone),
    /// boxed because the variant is large.
    pub fn deliver(
        &self,
        event: PullsyncEvent,
    ) -> Result<(), Box<mpsc::error::TrySendError<PullsyncEvent>>> {
        self.events.try_send(event).map_err(Box::new)
    }
}

/// Default event-channel capacity.
pub const DEFAULT_EVENT_CAPACITY: usize = 256;

impl<C, S, V, A, G, N, R> SpawnableTask for Puller<C, PullsyncEvent, S, V, A, G, N, R>
where
    C: PullsyncControl + 'static,
    S: IntervalStore + 'static,
    V: PullChunkVerifier + 'static,
    A: ReserveAdmit + 'static,
    G: ReadinessGate + Send + 'static,
    N: NeighbourSource + 'static,
    R: PeerReporter + 'static,
{
    fn into_task(
        self,
        shutdown: GracefulShutdown,
    ) -> impl std::future::Future<Output = ()> + MaybeSend {
        self.run(shutdown)
    }
}

/// A constructed [`Puller`] paired with the handle the node bridge feeds events
/// through.
pub type BuiltPuller<C, S, V, A, G, N, R> =
    (Puller<C, PullsyncEvent, S, V, A, G, N, R>, PullerHandle);

/// Build a puller plus its event handle, wiring an mpsc channel of the given
/// capacity between the node bridge and the loop.
pub fn build_puller<C, S, V, A, G, N, R>(
    seams: PullerSeams<C, S, V, A, G, N, R>,
    config: PullerConfig,
    event_capacity: usize,
) -> BuiltPuller<C, S, V, A, G, N, R>
where
    C: PullsyncControl,
    S: IntervalStore,
    V: PullChunkVerifier,
    A: ReserveAdmit,
    G: ReadinessGate,
    N: NeighbourSource,
    R: PeerReporter,
{
    let (events_tx, events_rx) = mpsc::channel(event_capacity);
    let puller = Puller::new(seams, events_rx, config);
    (puller, PullerHandle { events: events_tx })
}

/// Spawn the puller as a graceful-shutdown service, returning its event handle.
pub fn spawn_puller<C, S, V, A, G, N, R>(
    executor: &vertex_tasks::TaskExecutor,
    seams: PullerSeams<C, S, V, A, G, N, R>,
    config: PullerConfig,
) -> PullerHandle
where
    C: PullsyncControl + 'static,
    S: IntervalStore + 'static,
    V: PullChunkVerifier + 'static,
    A: ReserveAdmit + 'static,
    G: ReadinessGate + Send + 'static,
    N: NeighbourSource + 'static,
    R: PeerReporter + 'static,
{
    let (puller, handle) = build_puller(seams, config, DEFAULT_EVENT_CAPACITY);
    executor.spawn_service("swarm.puller", puller);
    handle
}
