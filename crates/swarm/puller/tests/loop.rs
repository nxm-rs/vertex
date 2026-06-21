//! Deterministic unit tests for the puller loop against mock seams.
//!
//! No real time, topology, or network: the event channel is pre-seeded with the
//! scripted behaviour responses and a single `sync_pass` is driven directly, so
//! the readiness gate and tail backoff are out of scope.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use alloy_primitives::{B256, Signature};
use libp2p::PeerId;
use nectar_postage::Stamp;
use nectar_primitives::ContentChunk;
use tokio::sync::mpsc;
use vertex_swarm_api::{
    IntervalStore, PeerReporter, PullChunkVerifier, ReportSource, SwarmResult, SwarmScoringEvent,
    VerifyError,
};
use vertex_swarm_primitives::{Bin, OverlayAddress, StampedChunk};
use vertex_swarm_puller::{
    NeighbourSource, Puller, PullerConfig, PullerSeams, PullsyncControl, PullsyncEvent,
    ReserveAdmit, SyncTarget,
};

// The readiness gate is exercised by `run`, not `sync_pass`; these tests drive
// one deterministic pass directly, so a unit gate stands in.
struct NoGate;
impl vertex_swarm_puller::ReadinessGate for NoGate {
    fn wait_ready(&self) -> impl std::future::Future<Output = ()> + Send {
        std::future::ready(())
    }
}

// ---- mock seams ----

// Each mock is a `Clone` newtype over shared state, so the trait impl is on a
// local type (no orphan-rule violation from impl-on-`Arc`).

#[derive(Clone, Default)]
struct MockControl {
    fetched: Arc<Mutex<Vec<PeerId>>>,
    ranges: Arc<Mutex<Vec<(PeerId, Bin, u64)>>>,
    /// Request ids stamped on each `sync_range`, in issue order, so a test can
    /// reply with the live id rather than guessing it.
    range_ids: Arc<Mutex<Vec<u64>>>,
}

impl PullsyncControl for MockControl {
    fn fetch_cursors(&self, peer: PeerId, _request_id: u64) {
        self.fetched.lock().unwrap().push(peer);
    }

    fn sync_range(&self, peer: PeerId, request_id: u64, bin: Bin, start: u64) {
        self.ranges.lock().unwrap().push((peer, bin, start));
        self.range_ids.lock().unwrap().push(request_id);
    }
}

/// In-memory [`IntervalStore`].
#[derive(Clone, Default)]
struct MockIntervals {
    intervals: Arc<Mutex<std::collections::HashMap<(OverlayAddress, u8), u64>>>,
    epochs: Arc<Mutex<std::collections::HashMap<OverlayAddress, u64>>>,
}

impl IntervalStore for MockIntervals {
    fn interval(&self, peer: &OverlayAddress, bin: Bin) -> SwarmResult<u64> {
        Ok(self
            .intervals
            .lock()
            .unwrap()
            .get(&(*peer, bin.get()))
            .copied()
            .unwrap_or(0))
    }

    fn set_interval(&self, peer: &OverlayAddress, bin: Bin, binid: u64) -> SwarmResult<()> {
        self.intervals
            .lock()
            .unwrap()
            .insert((*peer, bin.get()), binid);
        Ok(())
    }

    fn peer_epoch(&self, peer: &OverlayAddress) -> SwarmResult<Option<u64>> {
        Ok(self.epochs.lock().unwrap().get(peer).copied())
    }

    fn set_peer_epoch(&self, peer: &OverlayAddress, epoch: u64) -> SwarmResult<()> {
        self.epochs.lock().unwrap().insert(*peer, epoch);
        Ok(())
    }

    fn reset_peer(&self, peer: &OverlayAddress, epoch: u64) -> SwarmResult<()> {
        self.intervals.lock().unwrap().retain(|(p, _), _| p != peer);
        self.epochs.lock().unwrap().insert(*peer, epoch);
        Ok(())
    }
}

/// Records every admitted chunk address.
#[derive(Clone, Default)]
struct MockAdmit {
    admitted: Arc<Mutex<Vec<nectar_primitives::ChunkAddress>>>,
}

impl ReserveAdmit for MockAdmit {
    fn admit(&self, chunk: StampedChunk) -> SwarmResult<()> {
        self.admitted.lock().unwrap().push(*chunk.address());
        Ok(())
    }
}

/// Records every reported scoring event by reported overlay.
#[derive(Clone, Default)]
struct MockReporter {
    reports: Arc<Mutex<Vec<(OverlayAddress, SwarmScoringEvent, ReportSource)>>>,
}

impl PeerReporter for MockReporter {
    fn report_peer(
        &self,
        overlay: &OverlayAddress,
        event: SwarmScoringEvent,
        source: ReportSource,
    ) {
        self.reports.lock().unwrap().push((*overlay, event, source));
    }
}

/// Verifier with a fixed verdict for all chunks.
#[derive(Clone, Copy)]
struct FixedVerifier {
    accept: bool,
}

impl PullChunkVerifier for FixedVerifier {
    fn verify(&self, _chunk: &StampedChunk) -> Result<(), VerifyError> {
        if self.accept {
            Ok(())
        } else {
            Err(VerifyError::InvalidSignature)
        }
    }
}

struct OneTarget(SyncTarget);

impl NeighbourSource for OneTarget {
    fn targets(&self) -> Vec<SyncTarget> {
        vec![self.0.clone()]
    }
}

// ---- fixtures ----

fn overlay(n: u8) -> OverlayAddress {
    OverlayAddress::from([n; 32])
}

fn bin(n: u8) -> Bin {
    Bin::new(n).unwrap()
}

fn stamped(seed: u8) -> StampedChunk {
    let chunk = ContentChunk::new(vec![seed; 64]).unwrap();
    let mut raw = [0u8; 65];
    raw[..64].fill(1);
    raw[64] = 27;
    let sig = Signature::try_from(&raw[..]).unwrap();
    let stamp = Stamp::new(B256::repeat_byte(0xaa), 3, 7, 42, sig);
    StampedChunk::new(chunk.into(), stamp)
}

struct Harness {
    control: MockControl,
    intervals: MockIntervals,
    admit: MockAdmit,
    reporter: MockReporter,
    events_tx: mpsc::Sender<PullsyncEvent>,
    peer: PeerId,
    overlay: OverlayAddress,
}

/// Build a puller over the mocks and the scripted target, returning the puller
/// (caller spawns it) and the harness handles to assert against.
type TestPuller = Puller<
    MockControl,
    PullsyncEvent,
    MockIntervals,
    FixedVerifier,
    MockAdmit,
    NoGate,
    OneTarget,
    MockReporter,
>;

fn harness(accept: bool) -> (TestPuller, Harness) {
    let control = MockControl::default();
    let intervals = MockIntervals::default();
    let admit = MockAdmit::default();
    let reporter = MockReporter::default();
    let peer = PeerId::random();
    let ov = overlay(1);
    let target = SyncTarget {
        peer,
        overlay: ov,
        bins: vec![bin(2)],
    };
    let (events_tx, events_rx) = mpsc::channel(32);

    let puller = Puller::new(
        PullerSeams {
            control: control.clone(),
            intervals: intervals.clone(),
            verifier: FixedVerifier { accept },
            admit: admit.clone(),
            readiness: NoGate,
            neighbours: OneTarget(target),
            reporter: reporter.clone(),
        },
        events_rx,
        PullerConfig::default(),
    );

    (
        puller,
        Harness {
            control,
            intervals,
            admit,
            reporter,
            events_tx,
            peer,
            overlay: ov,
        },
    )
}

/// Seed the scripted events, then drive exactly one pass. The final caught-up
/// page makes the pass return without draining the channel dry.
async fn run_pass(mut puller: TestPuller, h: &Harness, events: Vec<PullsyncEvent>) {
    for e in events {
        h.events_tx.send(e).await.unwrap();
    }
    puller.sync_pass().await;
}

// ---- tests ----

#[tokio::test]
async fn non_empty_range_syncs_verifies_admits_and_advances() {
    let (puller, h) = harness(true);
    let chunk = stamped(0xaa);
    let addr = *chunk.address();

    run_pass(
        puller,
        &h,
        vec![
            // Cursors take request id 0, the first command of the pass.
            PullsyncEvent::CursorsReceived {
                peer: h.peer,
                request_id: 0,
                cursors: vec![],
                epoch: 1,
            },
            PullsyncEvent::RangeDelivered {
                peer: h.peer,
                request_id: 1,
                bin: bin(2),
                topmost: 10,
                chunks: vec![chunk],
            },
            // Caught up: topmost unchanged at the new resume point.
            PullsyncEvent::RangeDelivered {
                peer: h.peer,
                request_id: 2,
                bin: bin(2),
                topmost: 10,
                chunks: vec![],
            },
        ],
    )
    .await;

    assert_eq!(*h.control.fetched.lock().unwrap(), vec![h.peer]);
    // First sync_range starts at 0, second at the advanced 10.
    assert_eq!(
        *h.control.ranges.lock().unwrap(),
        vec![(h.peer, bin(2), 0), (h.peer, bin(2), 10)]
    );
    assert_eq!(*h.admit.admitted.lock().unwrap(), vec![addr]);
    assert_eq!(h.intervals.interval(&h.overlay, bin(2)).unwrap(), 10);
}

#[tokio::test]
async fn epoch_change_resets_intervals() {
    let (puller, h) = harness(true);
    // Pre-existing progress at a stale epoch.
    h.intervals.set_interval(&h.overlay, bin(2), 99).unwrap();
    h.intervals.set_peer_epoch(&h.overlay, 1).unwrap();

    run_pass(
        puller,
        &h,
        vec![
            // New epoch 2 differs from the stored 1: reset before syncing.
            PullsyncEvent::CursorsReceived {
                peer: h.peer,
                request_id: 0,
                cursors: vec![],
                epoch: 2,
            },
            // Empty page at the reset resume point: caught up immediately.
            PullsyncEvent::RangeDelivered {
                peer: h.peer,
                request_id: 1,
                bin: bin(2),
                topmost: 0,
                chunks: vec![],
            },
        ],
    )
    .await;

    assert_eq!(h.intervals.peer_epoch(&h.overlay).unwrap(), Some(2));
    // The stale interval was reset, and the first sync_range started at 0.
    assert_eq!(h.intervals.interval(&h.overlay, bin(2)).unwrap(), 0);
    assert_eq!(
        h.control.ranges.lock().unwrap().first().copied(),
        Some((h.peer, bin(2), 0))
    );
}

#[tokio::test]
async fn rejected_chunk_is_not_admitted_and_interval_not_advanced() {
    let (puller, h) = harness(false);
    let chunk = stamped(0xbb);

    run_pass(
        puller,
        &h,
        vec![
            PullsyncEvent::CursorsReceived {
                peer: h.peer,
                request_id: 0,
                cursors: vec![],
                epoch: 1,
            },
            PullsyncEvent::RangeDelivered {
                peer: h.peer,
                request_id: 1,
                bin: bin(2),
                topmost: 10,
                chunks: vec![chunk],
            },
        ],
    )
    .await;

    // Verifier rejected: nothing admitted and the interval is not advanced, so
    // the unverified span is retried on a later pass rather than skipped.
    assert!(h.admit.admitted.lock().unwrap().is_empty());
    assert_eq!(h.intervals.interval(&h.overlay, bin(2)).unwrap(), 0);
    // Exactly one sync_range was issued (from 0); the tainted page stops the bin.
    assert_eq!(*h.control.ranges.lock().unwrap(), vec![(h.peer, bin(2), 0)]);
    // The serving peer's overlay was reported for invalid data through pullsync.
    assert_eq!(
        *h.reporter.reports.lock().unwrap(),
        vec![(
            h.overlay,
            SwarmScoringEvent::InvalidData,
            ReportSource::Protocol("pullsync")
        )]
    );
}

// A late reply from a prior timed-out command stays buffered in the channel. It
// carries that command's (now stale) request id, so the next command's await
// must skip it rather than take it for its own delivery and advance the interval
// past data the new command has not yet received.
#[tokio::test]
async fn stale_range_reply_is_ignored() {
    let (puller, h) = harness(true);
    let chunk = stamped(0xcc);
    let addr = *chunk.address();

    run_pass(
        puller,
        &h,
        vec![
            PullsyncEvent::CursorsReceived {
                peer: h.peer,
                request_id: 0,
                cursors: vec![],
                epoch: 1,
            },
            // Buffered before the real reply: a stale page from a request id the
            // current `sync_range` (id 1) never issued. Its high topmost would
            // wrongly skip to 500 if matched on peer and bin alone.
            PullsyncEvent::RangeDelivered {
                peer: h.peer,
                request_id: 99,
                bin: bin(2),
                topmost: 500,
                chunks: vec![],
            },
            // The current command's own delivery.
            PullsyncEvent::RangeDelivered {
                peer: h.peer,
                request_id: 1,
                bin: bin(2),
                topmost: 10,
                chunks: vec![chunk],
            },
            // Caught up at the new resume point (request id 2).
            PullsyncEvent::RangeDelivered {
                peer: h.peer,
                request_id: 2,
                bin: bin(2),
                topmost: 10,
                chunks: vec![],
            },
        ],
    )
    .await;

    // The stale page was discarded: the interval advanced only to the id-1
    // delivery's topmost, and exactly its chunk was admitted.
    assert_eq!(*h.admit.admitted.lock().unwrap(), vec![addr]);
    assert_eq!(
        h.intervals.interval(&h.overlay, bin(2)).unwrap(),
        10,
        "the interval advances by the new command's delivery, not the stale 500"
    );
    // Two range commands issued (the id-1 fetch from 0, the id-2 catch-up from
    // 10); the stale reply triggered no extra command.
    assert_eq!(
        *h.control.ranges.lock().unwrap(),
        vec![(h.peer, bin(2), 0), (h.peer, bin(2), 10)]
    );
}

struct TwoTargets(Vec<SyncTarget>);

impl NeighbourSource for TwoTargets {
    fn targets(&self) -> Vec<SyncTarget> {
        self.0.clone()
    }
}

// A silent peer (never-connected or wedged) emits no `Failed` event, so the
// per-peer await would block the whole pass forever without the timeout. With
// paused time tokio auto-advances to the deadline once the await parks, so the
// pass abandons the silent first target and still processes the second.
#[tokio::test(start_paused = true)]
async fn silent_peer_is_abandoned_and_pass_proceeds() {
    let control = MockControl::default();
    let intervals = MockIntervals::default();
    let admit = MockAdmit::default();

    let silent = PeerId::random();
    let silent_ov = overlay(1);
    let live = PeerId::random();
    let live_ov = overlay(2);

    let targets = vec![
        SyncTarget {
            peer: silent,
            overlay: silent_ov,
            bins: vec![bin(2)],
        },
        SyncTarget {
            peer: live,
            overlay: live_ov,
            bins: vec![bin(2)],
        },
    ];

    let timeout = std::time::Duration::from_secs(45);
    let (events_tx, events_rx) = mpsc::channel(32);
    let puller = Puller::new(
        PullerSeams {
            control: control.clone(),
            intervals: intervals.clone(),
            verifier: FixedVerifier { accept: true },
            admit: admit.clone(),
            readiness: NoGate,
            neighbours: TwoTargets(targets),
            reporter: MockReporter::default(),
        },
        events_rx,
        PullerConfig {
            peer_response_timeout: timeout,
            ..PullerConfig::default()
        },
    );

    // The silent peer's await would discard any live-peer event already queued,
    // so the live page is delivered only once that await has elapsed and the pass
    // has advanced to the live peer. Driving the pass on a task lets the test body
    // step the paused clock past the silent peer's deadline first.
    let mut puller = puller;
    let handle = tokio::spawn(async move {
        puller.sync_pass().await;
        puller
    });

    // Let the silent peer's await park, then elapse its deadline.
    tokio::time::sleep(timeout + std::time::Duration::from_secs(1)).await;

    // The pass is now awaiting the live peer; deliver its cursor and caught-up
    // page. The silent peer consumed request id 0; the live peer's cursor takes
    // id 1 and its range id 2.
    events_tx
        .send(PullsyncEvent::CursorsReceived {
            peer: live,
            request_id: 1,
            cursors: vec![],
            epoch: 1,
        })
        .await
        .unwrap();
    events_tx
        .send(PullsyncEvent::RangeDelivered {
            peer: live,
            request_id: 2,
            bin: bin(2),
            topmost: 0,
            chunks: vec![],
        })
        .await
        .unwrap();

    handle.await.unwrap();

    // Both peers had cursors fetched: the pass did not wedge on the silent one.
    assert_eq!(
        *control.fetched.lock().unwrap(),
        vec![silent, live],
        "the pass must reach the second target after abandoning the first"
    );
    // The silent peer advanced no interval and issued no range; the live peer was
    // synced through to its caught-up page.
    assert_eq!(intervals.interval(&silent_ov, bin(2)).unwrap(), 0);
    assert!(intervals.peer_epoch(&silent_ov).unwrap().is_none());
    assert_eq!(
        *control.ranges.lock().unwrap(),
        vec![(live, bin(2), 0)],
        "only the live peer drove a range exchange"
    );
}

// Rejects only the one chunk at the poison address, so one peer's page fails
// verification while another's passes in the same pass.
#[derive(Clone, Copy)]
struct AddressRejectVerifier {
    poison: nectar_primitives::ChunkAddress,
}

impl PullChunkVerifier for AddressRejectVerifier {
    fn verify(&self, chunk: &StampedChunk) -> Result<(), VerifyError> {
        if *chunk.address() == self.poison {
            Err(VerifyError::InvalidSignature)
        } else {
            Ok(())
        }
    }
}

// A neighbour that serves an unverifiable chunk is reported for invalid data and
// skipped for the rest of the pass; a different neighbour in the same pass still
// syncs through to its caught-up page.
#[tokio::test]
async fn poison_peer_is_reported_and_skipped_other_peer_syncs() {
    let control = MockControl::default();
    let intervals = MockIntervals::default();
    let admit = MockAdmit::default();
    let reporter = MockReporter::default();

    let poison = PeerId::random();
    let poison_ov = overlay(1);
    let good = PeerId::random();
    let good_ov = overlay(2);

    let targets = vec![
        SyncTarget {
            peer: poison,
            overlay: poison_ov,
            bins: vec![bin(2)],
        },
        SyncTarget {
            peer: good,
            overlay: good_ov,
            bins: vec![bin(2)],
        },
    ];

    let bad_chunk = stamped(0xbb);
    let bad_addr = *bad_chunk.address();
    let good_chunk = stamped(0xaa);
    let good_addr = *good_chunk.address();

    let (events_tx, events_rx) = mpsc::channel(32);
    let mut puller = Puller::new(
        PullerSeams {
            control: control.clone(),
            intervals: intervals.clone(),
            verifier: AddressRejectVerifier { poison: bad_addr },
            admit: admit.clone(),
            readiness: NoGate,
            neighbours: TwoTargets(targets),
            reporter: reporter.clone(),
        },
        events_rx,
        PullerConfig::default(),
    );

    // Poison peer: cursor id 0, then a page (id 1) whose chunk fails verification.
    // The page taints the bin, so no id-2 catch-up is issued for this peer.
    events_tx
        .send(PullsyncEvent::CursorsReceived {
            peer: poison,
            request_id: 0,
            cursors: vec![],
            epoch: 1,
        })
        .await
        .unwrap();
    events_tx
        .send(PullsyncEvent::RangeDelivered {
            peer: poison,
            request_id: 1,
            bin: bin(2),
            topmost: 10,
            chunks: vec![bad_chunk],
        })
        .await
        .unwrap();
    // Good peer: cursor id 2, a verifiable page (id 3), then a caught-up page
    // (id 4) at the advanced resume point.
    events_tx
        .send(PullsyncEvent::CursorsReceived {
            peer: good,
            request_id: 2,
            cursors: vec![],
            epoch: 1,
        })
        .await
        .unwrap();
    events_tx
        .send(PullsyncEvent::RangeDelivered {
            peer: good,
            request_id: 3,
            bin: bin(2),
            topmost: 10,
            chunks: vec![good_chunk],
        })
        .await
        .unwrap();
    events_tx
        .send(PullsyncEvent::RangeDelivered {
            peer: good,
            request_id: 4,
            bin: bin(2),
            topmost: 10,
            chunks: vec![],
        })
        .await
        .unwrap();

    puller.sync_pass().await;

    // The poison peer was reported once for invalid data through pullsync.
    assert_eq!(
        *reporter.reports.lock().unwrap(),
        vec![(
            poison_ov,
            SwarmScoringEvent::InvalidData,
            ReportSource::Protocol("pullsync")
        )]
    );
    // It was skipped: no admit, no interval advance, exactly one range (from 0).
    assert_eq!(intervals.interval(&poison_ov, bin(2)).unwrap(), 0);
    // The good peer synced: its chunk admitted and its interval advanced to 10.
    assert_eq!(*admit.admitted.lock().unwrap(), vec![good_addr]);
    assert_eq!(intervals.interval(&good_ov, bin(2)).unwrap(), 10);
    // The poison peer drove one range (the tainted page); the good peer drove the
    // fetch from 0 and the catch-up from 10.
    assert_eq!(
        *control.ranges.lock().unwrap(),
        vec![(poison, bin(2), 0), (good, bin(2), 0), (good, bin(2), 10)]
    );
}
