//! Behaviour-level round-trip: a puller behaviour syncs cursors and a range page
//! from a syncer behaviour backed by the canonical mock [`PullStorage`].
#![allow(clippy::expect_used, clippy::indexing_slicing, clippy::get_first)]

use std::sync::Arc;
use std::time::Duration;

use alloy_primitives::{B256, Signature};
use futures::StreamExt;
use libp2p::swarm::SwarmEvent;
use nectar_postage::Stamp;
use nectar_primitives::{AnyChunk, Bin, ChunkAddress, ContentChunk};
use vertex_swarm_api::{PullStorage, StampedChunk};
use vertex_swarm_primitives::SwarmNodeType;
use vertex_swarm_storer_behaviour::{PullsyncBehaviour, PullsyncEvent};
use vertex_swarm_test_utils::MockStorage;
use vertex_swarm_test_utils::harness::{HarnessNode, connect_and_activate, seeded_node};

fn content(payload: &'static [u8]) -> StampedChunk {
    let sig = Signature::from_raw(&[1u8; 65]).expect("valid signature");
    let stamp = Stamp::new(B256::repeat_byte(0xaa), 3, 7, 42, sig);
    let chunk: AnyChunk = ContentChunk::new(payload)
        .expect("valid content chunk")
        .into();
    StampedChunk::new(chunk, stamp)
}

fn syncer(seed: u8, storage: MockStorage) -> HarnessNode<PullsyncBehaviour> {
    let storage: Arc<dyn PullStorage> = Arc::new(storage);
    seeded_node(seed, SwarmNodeType::Storer, move |_| {
        PullsyncBehaviour::new(storage)
    })
}

/// Drive both nodes until the puller emits a behaviour event, returning it. The
/// emitted event is the test's outcome, so it is captured directly rather than
/// asserted through accumulated swarm state.
async fn next_puller_event(
    puller: &mut HarnessNode<PullsyncBehaviour>,
    server: &mut HarnessNode<PullsyncBehaviour>,
    timeout: Duration,
) -> PullsyncEvent {
    tokio::time::timeout(timeout, async {
        loop {
            tokio::select! {
                _ = server.swarm.select_next_some() => {}
                ev = puller.swarm.select_next_some() => {
                    if let SwarmEvent::Behaviour(e) = ev {
                        return e;
                    }
                }
            }
        }
    })
    .await
    .expect("event resolved within timeout")
}

#[tokio::test(start_paused = true)]
async fn cursor_handshake_round_trips() {
    let bin = Bin::new(5).expect("valid bin");
    let chunks = vec![content(b"cursor chunk a"), content(b"cursor chunk b")];
    let mut puller = syncer(1, MockStorage::default());
    let mut server = syncer(2, MockStorage::with_chunks(bin, 7, chunks));
    let server_peer = server.peer_id;

    connect_and_activate(&mut puller, &mut server, |_, _, _, _| {}).await;
    puller.swarm.behaviour_mut().fetch_cursors(server_peer, 1);

    let event = next_puller_event(&mut puller, &mut server, Duration::from_secs(10)).await;

    match event {
        PullsyncEvent::CursorsReceived {
            peer,
            request_id,
            cursors,
            epoch,
        } => {
            assert_eq!(peer, server_peer);
            assert_eq!(request_id, 1, "the reply echoes the command request id");
            assert_eq!(epoch, 7);
            assert_eq!(cursors.len(), Bin::COUNT);
            assert_eq!(cursors.get(5), Some(&2), "bin 5 holds two entries");
            assert_eq!(cursors.get(0), Some(&0), "other bins are empty");
        }
        other => panic!("expected cursors, got {other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn range_exchange_delivers_the_page() {
    let bin = Bin::new(3).expect("valid bin");
    let chunks = vec![content(b"range chunk one"), content(b"range chunk two")];
    let addresses: Vec<ChunkAddress> = chunks.iter().map(|c| *c.address()).collect();
    let mut puller = syncer(1, MockStorage::default());
    let mut server = syncer(2, MockStorage::with_chunks(bin, 1, chunks));
    let server_peer = server.peer_id;

    connect_and_activate(&mut puller, &mut server, |_, _, _, _| {}).await;
    puller
        .swarm
        .behaviour_mut()
        .sync_range(server_peer, 2, bin, 0);

    let event = next_puller_event(&mut puller, &mut server, Duration::from_secs(10)).await;

    match event {
        PullsyncEvent::RangeDelivered {
            peer,
            request_id,
            bin: got_bin,
            topmost,
            chunks,
        } => {
            assert_eq!(peer, server_peer);
            assert_eq!(request_id, 2, "the reply echoes the command request id");
            assert_eq!(got_bin, bin);
            assert_eq!(topmost, 2, "topmost covers both entries");
            assert_eq!(chunks.len(), 2, "the whole page is wanted and delivered");
            let delivered: Vec<ChunkAddress> = chunks.iter().map(|c| *c.address()).collect();
            assert_eq!(delivered, addresses, "deliveries arrive in offer order");
        }
        other => panic!("expected a range delivery, got {other:?}"),
    }
}

/// An empty range completes promptly with topmost 0 and no want round.
#[tokio::test(start_paused = true)]
async fn empty_range_completes_with_no_want() {
    let bin = Bin::new(3).expect("valid bin");
    // The syncer holds chunks in a different bin, so the requested bin is empty.
    let other = Bin::new(4).expect("valid bin");
    let chunks = vec![content(b"elsewhere chunk")];
    let mut puller = syncer(1, MockStorage::default());
    let mut server = syncer(2, MockStorage::with_chunks(other, 1, chunks));
    let server_peer = server.peer_id;

    connect_and_activate(&mut puller, &mut server, |_, _, _, _| {}).await;
    puller
        .swarm
        .behaviour_mut()
        .sync_range(server_peer, 3, bin, 0);

    let event = next_puller_event(&mut puller, &mut server, Duration::from_secs(5)).await;

    match event {
        PullsyncEvent::RangeDelivered {
            peer,
            request_id,
            bin: got_bin,
            topmost,
            chunks,
        } => {
            assert_eq!(peer, server_peer);
            assert_eq!(request_id, 3, "the reply echoes the command request id");
            assert_eq!(got_bin, bin);
            assert_eq!(topmost, 0, "an empty range yields topmost 0, not start");
            assert!(chunks.is_empty(), "an empty range delivers no chunks");
        }
        other => panic!("expected an empty range delivery, got {other:?}"),
    }
}
