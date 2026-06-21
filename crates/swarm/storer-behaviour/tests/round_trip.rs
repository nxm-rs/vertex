//! Behaviour-level round-trip: a puller behaviour syncs cursors and a range page
//! from a syncer behaviour backed by a mock [`PullStorage`].
#![allow(clippy::expect_used, clippy::indexing_slicing, clippy::get_first)]

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
    BatchId, BinScanItem, PullStorage, StampedChunk, StorageRadius, SwarmResult,
};
use vertex_swarm_primitives::CachedChunk;
use vertex_swarm_storer_behaviour::{PullsyncBehaviour, PullsyncEvent};

/// A reserve snapshot for one bin: ordered entries plus an address index.
#[derive(Default)]
struct MockPullStorage {
    bin: u8,
    epoch: u64,
    items: Vec<BinScanItem>,
    chunks: HashMap<ChunkAddress, StampedChunk>,
}

impl MockPullStorage {
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

impl vertex_swarm_api::SwarmLocalStore for MockPullStorage {
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

impl vertex_swarm_api::ReserveStore for MockPullStorage {
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

impl vertex_swarm_api::BinCursorStore for MockPullStorage {
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

impl PullStorage for MockPullStorage {
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

fn syncer(storage: MockPullStorage) -> Swarm<PullsyncBehaviour> {
    let storage: Arc<dyn PullStorage> = Arc::new(storage);
    Swarm::new_ephemeral_tokio(move |_| PullsyncBehaviour::new(Arc::clone(&storage)))
}

/// Connect a puller and a syncer over an in-memory transport.
async fn connect(puller: &mut Swarm<PullsyncBehaviour>, syncer: &mut Swarm<PullsyncBehaviour>) {
    puller.listen().with_memory_addr_external().await;
    syncer.listen().with_memory_addr_external().await;
    puller.connect(syncer).await;
}

#[tokio::test]
async fn cursor_handshake_round_trips() {
    let bin = Bin::new(5).expect("valid bin");
    let chunks = vec![content(b"cursor chunk a"), content(b"cursor chunk b")];
    let mut puller = syncer(MockPullStorage::default());
    let mut server = syncer(MockPullStorage::with_chunks(bin, 7, chunks));
    let server_peer = *server.local_peer_id();

    connect(&mut puller, &mut server).await;
    puller.behaviour_mut().fetch_cursors(server_peer);

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
    .expect("cursors resolved within timeout");

    match event {
        PullsyncEvent::CursorsReceived {
            peer,
            cursors,
            epoch,
        } => {
            assert_eq!(peer, server_peer);
            assert_eq!(epoch, 7);
            assert_eq!(cursors.len(), Bin::COUNT);
            assert_eq!(cursors.get(5), Some(&2), "bin 5 holds two entries");
            assert_eq!(cursors.get(0), Some(&0), "other bins are empty");
        }
        other => panic!("expected cursors, got {other:?}"),
    }
}

#[tokio::test]
async fn range_exchange_delivers_the_page() {
    let bin = Bin::new(3).expect("valid bin");
    let chunks = vec![content(b"range chunk one"), content(b"range chunk two")];
    let addresses: Vec<ChunkAddress> = chunks.iter().map(|c| *c.address()).collect();
    let mut puller = syncer(MockPullStorage::default());
    let mut server = syncer(MockPullStorage::with_chunks(bin, 1, chunks));
    let server_peer = *server.local_peer_id();

    connect(&mut puller, &mut server).await;
    puller.behaviour_mut().sync_range(server_peer, bin, 0);

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
        PullsyncEvent::RangeDelivered {
            peer,
            bin: got_bin,
            topmost,
            chunks,
        } => {
            assert_eq!(peer, server_peer);
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
#[tokio::test]
async fn empty_range_completes_with_no_want() {
    let bin = Bin::new(3).expect("valid bin");
    // The syncer holds chunks in a different bin, so the requested bin is empty.
    let other = Bin::new(4).expect("valid bin");
    let chunks = vec![content(b"elsewhere chunk")];
    let mut puller = syncer(MockPullStorage::default());
    let mut server = syncer(MockPullStorage::with_chunks(other, 1, chunks));
    let server_peer = *server.local_peer_id();

    connect(&mut puller, &mut server).await;
    puller.behaviour_mut().sync_range(server_peer, bin, 0);

    let event = tokio::time::timeout(Duration::from_secs(5), async {
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
    .expect("empty range resolves promptly, no hang");

    match event {
        PullsyncEvent::RangeDelivered {
            peer,
            bin: got_bin,
            topmost,
            chunks,
        } => {
            assert_eq!(peer, server_peer);
            assert_eq!(got_bin, bin);
            assert_eq!(topmost, 0, "an empty range yields topmost 0, not start");
            assert!(chunks.is_empty(), "an empty range delivers no chunks");
        }
        other => panic!("expected an empty range delivery, got {other:?}"),
    }
}
