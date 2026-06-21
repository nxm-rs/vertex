//! `StorerBehaviour` composite: event routing through `StorerBehaviourEvent`,
//! and a compose-connect-poll exercising the pullsync sub-behaviour through the
//! derived composite.
#![allow(clippy::expect_used, clippy::indexing_slicing)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use alloy_primitives::{B256, Signature};
use futures::StreamExt;
use libp2p::Swarm;
use libp2p_swarm_test::SwarmExt;
use nectar_postage::Stamp;
use nectar_primitives::{AnyChunk, Bin, ChunkAddress, ContentChunk, ProximityOrder};
use vertex_swarm_api::{
    BatchId, BinScanItem, PullStorage, StampedChunk, StorageRadius, SwarmLocalStore, SwarmResult,
};
use vertex_swarm_client_behaviour::{
    BehaviourConfig as ClientBehaviourConfig, ClientBehaviour, StubForwarder,
};
use vertex_swarm_primitives::CachedChunk;
use vertex_swarm_storer_behaviour::{
    PullsyncBehaviour, PullsyncEvent, StorerBehaviour, StorerBehaviourEvent,
};

/// A reserve snapshot for one bin: ordered entries plus an address index. Serves
/// as both the client store and the pullsync server snapshot.
#[derive(Default)]
struct MockStorage {
    bin: u8,
    epoch: u64,
    items: Vec<BinScanItem>,
    chunks: HashMap<ChunkAddress, StampedChunk>,
}

impl MockStorage {
    fn with_chunks(bin: Bin, epoch: u64, chunks: Vec<StampedChunk>) -> Self {
        let mut items = Vec::new();
        let mut index = HashMap::new();
        for (i, chunk) in chunks.into_iter().enumerate() {
            let address = *chunk.address();
            let stamp_hash = B256::from_slice(address.as_slice());
            items.push(BinScanItem {
                seq: i as u64 + 1,
                address,
                batch_id: BatchId::repeat_byte(0xbb),
                stamp_hash,
            });
            index.insert(address, chunk);
        }
        Self {
            bin: bin.get(),
            epoch,
            items,
            chunks: index,
        }
    }
}

impl SwarmLocalStore for MockStorage {
    fn put(&self, _chunk: CachedChunk) -> SwarmResult<()> {
        Ok(())
    }

    fn get(&self, address: &ChunkAddress) -> SwarmResult<Option<CachedChunk>> {
        Ok(self.chunks.get(address).cloned().map(CachedChunk::from))
    }

    fn contains(&self, address: &ChunkAddress) -> bool {
        self.chunks.contains_key(address)
    }

    fn remove(&self, _address: &ChunkAddress) -> SwarmResult<()> {
        Ok(())
    }
}

impl vertex_swarm_api::ReserveStore for MockStorage {
    fn storage_radius(&self) -> StorageRadius {
        StorageRadius::ZERO
    }

    fn is_responsible_for(&self, _address: &ChunkAddress) -> bool {
        true
    }

    fn count(&self) -> SwarmResult<u64> {
        Ok(self.items.len() as u64)
    }

    fn capacity(&self) -> u64 {
        u64::MAX
    }

    fn count_in(&self, _po: ProximityOrder) -> SwarmResult<u64> {
        Ok(0)
    }

    fn evict_furthest(&self) -> SwarmResult<Option<ChunkAddress>> {
        Ok(None)
    }

    fn evict_from_bin(&self, _bin: Bin, _max: u64) -> SwarmResult<u64> {
        Ok(0)
    }

    fn evict_batch(&self, _batch: BatchId, _up_to_bin: Option<Bin>, _max: u64) -> SwarmResult<u64> {
        Ok(0)
    }
}

impl vertex_swarm_api::BinCursorStore for MockStorage {
    fn bin_cursor(&self, bin: Bin) -> SwarmResult<u64> {
        if bin.get() == self.bin {
            Ok(self.items.last().map(|i| i.seq).unwrap_or(0))
        } else {
            Ok(0)
        }
    }

    fn scan_bin_from<'a>(
        &'a self,
        bin: Bin,
        start_seq: u64,
    ) -> SwarmResult<Box<dyn Iterator<Item = SwarmResult<BinScanItem>> + Send + 'a>> {
        let items: Vec<BinScanItem> = if bin.get() == self.bin {
            self.items
                .iter()
                .filter(|i| i.seq >= start_seq)
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        Ok(Box::new(items.into_iter().map(Ok)))
    }
}

impl PullStorage for MockStorage {
    fn reserve_epoch(&self) -> u64 {
        self.epoch
    }
}

fn content(payload: &'static [u8]) -> StampedChunk {
    let sig = Signature::from_raw(&[1u8; 65]).expect("valid signature");
    let stamp = Stamp::new(B256::repeat_byte(0xaa), 3, 7, 42, sig);
    let chunk: AnyChunk = ContentChunk::new(payload)
        .expect("valid content chunk")
        .into();
    StampedChunk::new(chunk, stamp)
}

fn storer(storage: MockStorage) -> Swarm<StorerBehaviour> {
    let storage = Arc::new(storage);
    let store = Arc::clone(&storage);
    Swarm::new_ephemeral_tokio(move |_| {
        let client = ClientBehaviour::new(
            ClientBehaviourConfig::default(),
            store.clone(),
            Arc::new(StubForwarder),
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
        cursors: vec![0, 1, 2],
        epoch: 7,
    };
    let lifted: StorerBehaviourEvent = event.into();
    assert!(matches!(lifted, StorerBehaviourEvent::Pullsync(_)));
}

/// The composite forwards pullsync range deliveries through
/// `StorerBehaviourEvent::Pullsync`, proving the derived multiplexer routes a
/// sub-behaviour's events.
#[tokio::test]
async fn composite_routes_pullsync_range() {
    let bin = Bin::new(3).expect("valid bin");
    let chunks = vec![content(b"range chunk one"), content(b"range chunk two")];
    let addresses: Vec<ChunkAddress> = chunks.iter().map(|c| *c.address()).collect();
    let mut puller = storer(MockStorage::default());
    let mut server = storer(MockStorage::with_chunks(bin, 1, chunks));
    let server_peer = *server.local_peer_id();

    puller.listen().with_memory_addr_external().await;
    server.listen().with_memory_addr_external().await;
    puller.connect(&mut server).await;

    puller
        .behaviour_mut()
        .pullsync
        .sync_range(server_peer, bin, 0);

    let event = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            tokio::select! {
                _ = server.select_next_some() => {}
                ev = puller.select_next_some() => {
                    if let libp2p::swarm::SwarmEvent::Behaviour(e) = ev {
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
            bin: got_bin,
            topmost,
            chunks,
        }) => {
            assert_eq!(peer, server_peer);
            assert_eq!(got_bin, bin);
            assert_eq!(topmost, 2);
            let delivered: Vec<ChunkAddress> = chunks.iter().map(|c| *c.address()).collect();
            assert_eq!(delivered, addresses);
        }
        other => panic!("expected a pullsync range delivery, got {other:?}"),
    }
}
