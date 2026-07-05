//! `StorerBehaviour` composite: event routing through `StorerBehaviourEvent`,
//! and a compose-connect-poll exercising the pullsync sub-behaviour through the
//! derived composite.
#![allow(clippy::expect_used, clippy::indexing_slicing)]

use std::sync::Arc;
use std::time::Duration;

use alloy_primitives::{B256, Signature};
use futures::StreamExt;
use libp2p::swarm::SwarmEvent;
use nectar_postage::Stamp;
use nectar_primitives::{AnyChunk, Bin, ChunkAddress, ContentChunk};
use vertex_swarm_api::{PullStorage, StampedChunk, SwarmLocalStore};
use vertex_swarm_client_behaviour::{
    BehaviourConfig as ClientBehaviourConfig, ClientBehaviour, StubForwarder,
};
use vertex_swarm_primitives::SwarmNodeType;
use vertex_swarm_storer_behaviour::{
    PullsyncBehaviour, PullsyncEvent, StorerBehaviour, StorerBehaviourEvent,
};
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

fn storer(seed: u8, storage: MockStorage) -> HarnessNode<StorerBehaviour> {
    let storage = Arc::new(storage);
    seeded_node(seed, SwarmNodeType::Storer, move |ctx| {
        let client = ClientBehaviour::new(
            ClientBehaviourConfig::default(),
            Arc::clone(&storage) as Arc<dyn SwarmLocalStore>,
            Arc::new(StubForwarder),
            ctx.identities.clone(),
        );
        let pullsync = PullsyncBehaviour::new(Arc::clone(&storage) as Arc<dyn PullStorage>);
        StorerBehaviour { client, pullsync }
    })
}

/// A `PullsyncEvent` lifts into the composite event under the `Pullsync` arm.
#[test]
fn event_routes_to_pullsync_arm() {
    let event = PullsyncEvent::CursorsReceived {
        peer: libp2p::PeerId::random(),
        request_id: 1,
        cursors: vec![0, 1, 2],
        epoch: 7,
    };
    let lifted: StorerBehaviourEvent = event.into();
    assert!(matches!(lifted, StorerBehaviourEvent::Pullsync(_)));
}

/// The composite forwards pullsync range deliveries through
/// `StorerBehaviourEvent::Pullsync`, proving the derived multiplexer routes a
/// sub-behaviour's events.
#[tokio::test(start_paused = true)]
async fn composite_routes_pullsync_range() {
    let bin = Bin::new(3).expect("valid bin");
    let chunks = vec![content(b"range chunk one"), content(b"range chunk two")];
    let addresses: Vec<ChunkAddress> = chunks.iter().map(|c| *c.address()).collect();
    let mut puller = storer(1, MockStorage::default());
    let mut server = storer(2, MockStorage::with_chunks(bin, 1, chunks));
    let server_peer = server.peer_id;

    connect_and_activate(&mut puller, &mut server, |_, _, _, _| {}).await;

    puller
        .swarm
        .behaviour_mut()
        .pullsync
        .sync_range(server_peer, 1, bin, 0);

    let event = tokio::time::timeout(Duration::from_secs(10), async {
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
    .expect("range resolved within timeout");

    match event {
        StorerBehaviourEvent::Pullsync(PullsyncEvent::RangeDelivered {
            peer,
            request_id,
            bin: got_bin,
            topmost,
            chunks,
        }) => {
            assert_eq!(peer, server_peer);
            assert_eq!(request_id, 1, "the reply echoes the command request id");
            assert_eq!(got_bin, bin);
            assert_eq!(topmost, 2);
            let delivered: Vec<ChunkAddress> = chunks.iter().map(|c| *c.address()).collect();
            assert_eq!(delivered, addresses);
        }
        other => panic!("expected a pullsync range delivery, got {other:?}"),
    }
}
