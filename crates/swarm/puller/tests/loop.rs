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
use vertex_swarm_api::{IntervalStore, PullChunkVerifier, SwarmResult, VerifyError};
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
}

impl PullsyncControl for MockControl {
    fn fetch_cursors(&self, peer: PeerId) {
        self.fetched.lock().unwrap().push(peer);
    }

    fn sync_range(&self, peer: PeerId, bin: Bin, start: u64) {
        self.ranges.lock().unwrap().push((peer, bin, start));
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
    events_tx: mpsc::Sender<PullsyncEvent>,
    peer: PeerId,
    overlay: OverlayAddress,
}

/// Build a puller over the mocks and the scripted target, returning the puller
/// (caller spawns it) and the harness handles to assert against.
type TestPuller =
    Puller<MockControl, PullsyncEvent, MockIntervals, FixedVerifier, MockAdmit, NoGate, OneTarget>;

fn harness(accept: bool) -> (TestPuller, Harness) {
    let control = MockControl::default();
    let intervals = MockIntervals::default();
    let admit = MockAdmit::default();
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
            PullsyncEvent::CursorsReceived {
                peer: h.peer,
                cursors: vec![],
                epoch: 1,
            },
            PullsyncEvent::RangeDelivered {
                peer: h.peer,
                bin: bin(2),
                topmost: 10,
                chunks: vec![chunk],
            },
            // Caught up: topmost unchanged at the new resume point.
            PullsyncEvent::RangeDelivered {
                peer: h.peer,
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
                cursors: vec![],
                epoch: 2,
            },
            // Empty page at the reset resume point: caught up immediately.
            PullsyncEvent::RangeDelivered {
                peer: h.peer,
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
                cursors: vec![],
                epoch: 1,
            },
            PullsyncEvent::RangeDelivered {
                peer: h.peer,
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

    // The pass is now awaiting the live peer; deliver its cursor and caught-up page.
    events_tx
        .send(PullsyncEvent::CursorsReceived {
            peer: live,
            cursors: vec![],
            epoch: 1,
        })
        .await
        .unwrap();
    events_tx
        .send(PullsyncEvent::RangeDelivered {
            peer: live,
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
