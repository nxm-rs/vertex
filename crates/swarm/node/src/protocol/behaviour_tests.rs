//! Behaviour-level integration tests for the re-exported [`ClientBehaviour`]
//! driven through the node-local [`NetworkForwarder`].
//!
//! These exercise the real libp2p handler over `libp2p-swarm-test`: cache
//! serving, the stub-forwarder reset paths, storer ingest (store and sign), and
//! the three-node relay that needs the concrete forwarder. They live in the node
//! crate because the relay tests construct a `NetworkForwarder` (which couples to
//! accounting and the outbound `ClientHandle`) over the behaviour the
//! `vertex-swarm-client-behaviour` crate provides.

use std::sync::Arc;
use std::time::Duration;

use alloy_primitives::{B256, Signature};
use alloy_signer_local::PrivateKeySigner;
use futures::StreamExt;
use libp2p::Swarm;
use libp2p_swarm_test::SwarmExt;
use nectar_postage::Stamp;
use nectar_primitives::{AnyChunk, ContentChunk, SingleOwnerChunk};
use tokio::sync::oneshot;
use vertex_swarm_api::SwarmLocalStore;
use vertex_swarm_localstore::{ChunkStore, Clock};
use vertex_swarm_primitives::{OverlayAddress, StampedChunk, SwarmNodeType};

use crate::ChunkTransferError;
use crate::client_service::RetrievalResult;
use crate::protocol::{BehaviourConfig as Config, ClientBehaviour, ClientCommand, StubForwarder};

/// Fixed-instant clock for SOC freshness tests.
struct FixedClock(i64);

impl Clock for FixedClock {
    fn now_ns(&self) -> i64 {
        self.0
    }
}

fn content_chunk(payload: &'static [u8]) -> StampedChunk {
    let sig = Signature::from_raw(&[1u8; 65]).expect("valid signature");
    let stamp = Stamp::new(B256::repeat_byte(0xaa), 3, 7, 42, sig);
    let chunk: AnyChunk = ContentChunk::new(payload)
        .expect("valid content chunk")
        .into();
    StampedChunk::new(chunk, stamp)
}

fn soc_chunk(payload: &'static [u8], stamp_ns: u64) -> StampedChunk {
    let sig = Signature::from_raw(&[1u8; 65]).expect("valid signature");
    let stamp = Stamp::new(B256::repeat_byte(0xaa), 3, 7, stamp_ns, sig);
    let signer = PrivateKeySigner::from_bytes(&B256::repeat_byte(0x11)).expect("signer");
    let chunk: AnyChunk = SingleOwnerChunk::new(B256::repeat_byte(0x22), payload, &signer)
        .expect("valid soc")
        .into();
    StampedChunk::new(chunk, stamp)
}

fn overlay(n: u8) -> OverlayAddress {
    OverlayAddress::from([n; 32])
}

fn swarm_with_store(store: Arc<dyn SwarmLocalStore>) -> Swarm<ClientBehaviour> {
    Swarm::new_ephemeral_tokio(move |_| {
        ClientBehaviour::new(
            Config::for_role(SwarmNodeType::Client),
            store,
            Arc::new(StubForwarder),
        )
    })
}

/// Connect two client swarms and activate both handlers so the
/// request/serve path is live.
async fn connect_and_activate(
    client: &mut Swarm<ClientBehaviour>,
    server: &mut Swarm<ClientBehaviour>,
    client_overlay: OverlayAddress,
    server_overlay: OverlayAddress,
) {
    let client_peer = *client.local_peer_id();
    let server_peer = *server.local_peer_id();
    client.listen().with_memory_addr_external().await;
    server.listen().with_memory_addr_external().await;
    client.connect(server).await;

    client
        .behaviour_mut()
        .on_command(ClientCommand::ActivatePeer {
            peer_id: server_peer,
            overlay: server_overlay,
            node_type: SwarmNodeType::Client,
        });
    server
        .behaviour_mut()
        .on_command(ClientCommand::ActivatePeer {
            peer_id: client_peer,
            overlay: client_overlay,
            node_type: SwarmNodeType::Client,
        });
}

async fn drive_until_retrieved(
    client: &mut Swarm<ClientBehaviour>,
    server: &mut Swarm<ClientBehaviour>,
    mut rx: oneshot::Receiver<Result<RetrievalResult, ChunkTransferError>>,
) -> Result<RetrievalResult, ChunkTransferError> {
    let drive = async {
        loop {
            tokio::select! {
                _ = client.select_next_some() => {}
                _ = server.select_next_some() => {}
                res = &mut rx => return res.expect("sender not dropped"),
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(10), drive)
        .await
        .expect("retrieval resolved within timeout")
}

#[tokio::test]
async fn serves_a_content_chunk_from_the_cache() {
    let chunk = content_chunk(b"served from cache");
    let address = *chunk.address();

    let server_store: Arc<dyn SwarmLocalStore> =
        Arc::new(ChunkStore::with_budget(1 << 20, 1_000_000_000));
    server_store.put(chunk.clone().into()).unwrap();

    let mut client = swarm_with_store(Arc::new(ChunkStore::with_budget(1 << 20, 1_000)));
    let mut server = swarm_with_store(server_store);

    let server_overlay = overlay(2);
    connect_and_activate(&mut client, &mut server, overlay(1), server_overlay).await;

    let (tx, rx) = oneshot::channel();
    client
        .behaviour_mut()
        .on_command(ClientCommand::RetrieveChunk {
            peer: server_overlay,
            address,
            response: tx,
        });

    let result = drive_until_retrieved(&mut client, &mut server, rx).await;
    let delivered = result.expect("served from cache");
    assert_eq!(*delivered.chunk.address(), address);
    assert_eq!(delivered.chunk, *chunk.chunk());
}

#[tokio::test]
async fn serves_a_fresh_soc_from_the_cache() {
    // SOC stamped at 900ns, served at 1000ns under a 500ns TTL: still fresh.
    let chunk = soc_chunk(b"feed v1", 900);
    let address = *chunk.address();

    let server_store: Arc<dyn SwarmLocalStore> = Arc::new(ChunkStore::with_budget_and_clock(
        1 << 20,
        500,
        FixedClock(1_000),
    ));
    server_store.put(chunk.clone().into()).unwrap();

    let mut client = swarm_with_store(Arc::new(ChunkStore::with_budget(1 << 20, 1_000)));
    let mut server = swarm_with_store(server_store);

    let server_overlay = overlay(2);
    connect_and_activate(&mut client, &mut server, overlay(1), server_overlay).await;

    let (tx, rx) = oneshot::channel();
    client
        .behaviour_mut()
        .on_command(ClientCommand::RetrieveChunk {
            peer: server_overlay,
            address,
            response: tx,
        });

    let delivered = drive_until_retrieved(&mut client, &mut server, rx)
        .await
        .expect("fresh SOC served from cache");
    assert_eq!(delivered.chunk, *chunk.chunk());
}

#[tokio::test]
async fn expired_soc_is_not_served_and_resets() {
    // SOC stamped at 900ns, served at 2000ns under a 500ns TTL: expired, so
    // the cache misses and the inbound retrieval resets rather than serving a
    // stale revision.
    let chunk = soc_chunk(b"feed v1", 900);
    let address = *chunk.address();

    let server_store: Arc<dyn SwarmLocalStore> = Arc::new(ChunkStore::with_budget_and_clock(
        1 << 20,
        500,
        FixedClock(2_000),
    ));
    server_store.put(chunk.into()).unwrap();

    let mut client = swarm_with_store(Arc::new(ChunkStore::with_budget(1 << 20, 1_000)));
    let mut server = swarm_with_store(server_store);

    let server_overlay = overlay(2);
    connect_and_activate(&mut client, &mut server, overlay(1), server_overlay).await;

    let (tx, rx) = oneshot::channel();
    client
        .behaviour_mut()
        .on_command(ClientCommand::RetrieveChunk {
            peer: server_overlay,
            address,
            response: tx,
        });

    let result = drive_until_retrieved(&mut client, &mut server, rx).await;
    assert!(
        result.is_err(),
        "an expired SOC must not be served; the stream resets so the requester forwards"
    );
}

#[tokio::test]
async fn cache_miss_resets_with_stub_forwarder() {
    // Empty cache plus stub forwarder: the inbound retrieval can neither
    // serve nor forward, so the substream resets and the requester fails.
    let address = *content_chunk(b"never cached").address();

    let mut client = swarm_with_store(Arc::new(ChunkStore::with_budget(1 << 20, 1_000)));
    let mut server = swarm_with_store(Arc::new(ChunkStore::with_budget(1 << 20, 1_000)));

    let server_overlay = overlay(2);
    connect_and_activate(&mut client, &mut server, overlay(1), server_overlay).await;

    let (tx, rx) = oneshot::channel();
    client
        .behaviour_mut()
        .on_command(ClientCommand::RetrieveChunk {
            peer: server_overlay,
            address,
            response: tx,
        });

    let result = drive_until_retrieved(&mut client, &mut server, rx).await;
    assert!(
        result.is_err(),
        "a cache miss with the stub forwarder must reset the stream"
    );
}

#[tokio::test]
async fn inbound_pushsync_resets_with_stub_forwarder() {
    // A cache-only client never takes custody: inbound pushsync forwards, the
    // stub forward fails, the substream resets, and no receipt is signed.
    let chunk = content_chunk(b"pushed chunk");

    let mut client = swarm_with_store(Arc::new(ChunkStore::with_budget(1 << 20, 1_000)));
    let mut server = swarm_with_store(Arc::new(ChunkStore::with_budget(1 << 20, 1_000)));

    let server_overlay = overlay(2);
    connect_and_activate(&mut client, &mut server, overlay(1), server_overlay).await;

    let (tx, mut rx) = oneshot::channel();
    client.behaviour_mut().on_command(ClientCommand::PushChunk {
        peer: server_overlay,
        address: *chunk.address(),
        chunk,
        response: tx,
    });

    let drive = async {
        loop {
            tokio::select! {
                _ = client.select_next_some() => {}
                _ = server.select_next_some() => {}
                res = &mut rx => return res.expect("sender not dropped"),
            }
        }
    };
    let result = tokio::time::timeout(Duration::from_secs(10), drive)
        .await
        .expect("push resolved within timeout");
    assert!(
        result.is_err(),
        "an inbound pushsync with the stub forwarder must reset the stream"
    );
}

// --- Storer ingest (store + sign) integration tests ---
//
// A storer holds a `StorerCapability`: a responsible delivery is stored and
// acknowledged with a signed receipt; a non-responsible delivery forwards
// (verbatim-relay), which resets under the stub forwarder. Both branches run
// through the real libp2p handler.

/// In-memory `ReserveStore` for the ingest tests. `is_responsible_for` is a
/// fixed flag so a test can pin either branch; the proximity-axis accounting
/// methods are stubs since the ingest path only uses `is_responsible_for`,
/// `put`, and `storage_radius`.
struct MockReserve {
    chunks: std::sync::Mutex<
        std::collections::HashMap<
            nectar_primitives::ChunkAddress,
            vertex_swarm_primitives::CachedChunk,
        >,
    >,
    responsible: bool,
    radius: vertex_swarm_api::StorageRadius,
}

impl MockReserve {
    fn new(responsible: bool, radius: vertex_swarm_api::StorageRadius) -> Self {
        Self {
            chunks: std::sync::Mutex::new(std::collections::HashMap::new()),
            responsible,
            radius,
        }
    }
}

impl SwarmLocalStore for MockReserve {
    fn put(
        &self,
        chunk: vertex_swarm_primitives::CachedChunk,
    ) -> vertex_swarm_api::SwarmResult<()> {
        self.chunks.lock().unwrap().insert(*chunk.address(), chunk);
        Ok(())
    }
    fn get(
        &self,
        address: &nectar_primitives::ChunkAddress,
    ) -> vertex_swarm_api::SwarmResult<Option<vertex_swarm_primitives::CachedChunk>> {
        Ok(self.chunks.lock().unwrap().get(address).cloned())
    }
    fn contains(&self, address: &nectar_primitives::ChunkAddress) -> bool {
        self.chunks.lock().unwrap().contains_key(address)
    }
    fn remove(
        &self,
        address: &nectar_primitives::ChunkAddress,
    ) -> vertex_swarm_api::SwarmResult<()> {
        self.chunks.lock().unwrap().remove(address);
        Ok(())
    }
}

impl vertex_swarm_api::ReserveStore for MockReserve {
    fn storage_radius(&self) -> vertex_swarm_api::StorageRadius {
        self.radius
    }
    fn is_responsible_for(&self, _address: &nectar_primitives::ChunkAddress) -> bool {
        self.responsible
    }
    fn count(&self) -> vertex_swarm_api::SwarmResult<u64> {
        Ok(self.chunks.lock().unwrap().len() as u64)
    }
    fn capacity(&self) -> u64 {
        u64::MAX
    }
    fn count_in(
        &self,
        _po: nectar_primitives::ProximityOrder,
    ) -> vertex_swarm_api::SwarmResult<u64> {
        Ok(0)
    }
    fn evict_furthest(
        &self,
    ) -> vertex_swarm_api::SwarmResult<Option<nectar_primitives::ChunkAddress>> {
        Ok(None)
    }
    fn evict_from_bin(
        &self,
        _bin: nectar_primitives::Bin,
        _max: u64,
    ) -> vertex_swarm_api::SwarmResult<u64> {
        Ok(0)
    }
    fn evict_batch(
        &self,
        _batch: vertex_swarm_primitives::BatchId,
        _up_to_bin: Option<nectar_primitives::Bin>,
        _max: u64,
    ) -> vertex_swarm_api::SwarmResult<u64> {
        Ok(0)
    }
}

/// Server swarm holding the storer ingest capability. Returns the swarm, the
/// shared reserve (to assert what was stored), the signer and nonce (to
/// assert the receipt recovers to the storer's overlay).
fn storer_swarm(
    responsible: bool,
    radius: vertex_swarm_api::StorageRadius,
) -> (
    Swarm<ClientBehaviour>,
    Arc<MockReserve>,
    alloy_signer_local::PrivateKeySigner,
    vertex_swarm_primitives::Nonce,
) {
    use alloy_signer_local::PrivateKeySigner;
    use nectar_primitives::NetworkId;
    use vertex_swarm_primitives::Nonce;

    let reserve = Arc::new(MockReserve::new(responsible, radius));
    let signer = PrivateKeySigner::random();
    let nonce = Nonce::from([0x5a; 32]);

    let reserve_for_swarm = Arc::clone(&reserve);
    let signer_for_swarm = signer.clone();
    let swarm = Swarm::new_ephemeral_tokio(move |_| {
        // The reserve serves on retrieval too, so it is the behaviour's store.
        let store: Arc<dyn SwarmLocalStore> = Arc::clone(&reserve_for_swarm) as _;
        let mut behaviour = ClientBehaviour::new(
            Config::for_role(SwarmNodeType::Storer),
            store,
            Arc::new(StubForwarder),
        );
        behaviour.set_network_id(NetworkId::MAINNET);
        let capability = crate::protocol::StorerCapability::new(
            Arc::clone(&reserve_for_swarm) as Arc<dyn vertex_swarm_api::ReserveStore>,
            Arc::new(signer_for_swarm.clone()) as Arc<dyn alloy_signer::SignerSync + Send + Sync>,
            NetworkId::MAINNET,
            nonce,
        );
        behaviour.set_storer(capability);
        behaviour
    });
    (swarm, reserve, signer, nonce)
}

#[tokio::test]
async fn responsible_storer_stores_and_signs_a_receipt() {
    use nectar_primitives::{NetworkId, compute_overlay};
    use vertex_swarm_api::StorageRadius;
    use vertex_swarm_primitives::Bin;

    let chunk = content_chunk(b"stored by the responsible storer");
    let address = *chunk.address();
    let radius = StorageRadius::new(Bin::new(4).unwrap());

    let (mut storer, reserve, signer, nonce) = storer_swarm(true, radius);
    let mut pusher = swarm_with_store(Arc::new(ChunkStore::with_budget(1 << 20, 1_000)));

    let storer_overlay = overlay(2);
    connect_and_activate(&mut pusher, &mut storer, overlay(1), storer_overlay).await;

    let (tx, mut rx) = oneshot::channel();
    pusher.behaviour_mut().on_command(ClientCommand::PushChunk {
        peer: storer_overlay,
        address,
        chunk,
        response: tx,
    });

    let drive = async {
        loop {
            tokio::select! {
                _ = pusher.select_next_some() => {}
                _ = storer.select_next_some() => {}
                res = &mut rx => return res.expect("sender not dropped"),
            }
        }
    };
    let result = tokio::time::timeout(Duration::from_secs(10), drive)
        .await
        .expect("push resolved within timeout");

    let receipt = result.expect("the responsible storer signs and returns a receipt");
    // The receipt acknowledges the chunk, declares the storer's radius, and
    // recovers to the storer's own overlay.
    assert_eq!(receipt.address, address);
    assert_eq!(receipt.storage_radius, radius);
    let expected_storer = compute_overlay(&signer.address(), NetworkId::MAINNET, &nonce);
    assert_eq!(
        receipt.storer, expected_storer,
        "the receipt recovers to the storer that signed it"
    );
    assert!(
        reserve.contains(&address),
        "the responsible storer took custody of the chunk"
    );
}

#[tokio::test]
async fn non_responsible_storer_forwards_instead_of_storing() {
    use vertex_swarm_api::StorageRadius;
    use vertex_swarm_primitives::Bin;

    // The storer holds the ingest capability but is NOT responsible, so it
    // forwards; the stub forward fails, the substream resets, nothing is
    // stored, and no receipt is signed.
    let chunk = content_chunk(b"not my responsibility");
    let address = *chunk.address();
    let radius = StorageRadius::new(Bin::new(4).unwrap());

    let (mut storer, reserve, _signer, _nonce) = storer_swarm(false, radius);
    let mut pusher = swarm_with_store(Arc::new(ChunkStore::with_budget(1 << 20, 1_000)));

    let storer_overlay = overlay(2);
    connect_and_activate(&mut pusher, &mut storer, overlay(1), storer_overlay).await;

    let (tx, mut rx) = oneshot::channel();
    pusher.behaviour_mut().on_command(ClientCommand::PushChunk {
        peer: storer_overlay,
        address,
        chunk,
        response: tx,
    });

    let drive = async {
        loop {
            tokio::select! {
                _ = pusher.select_next_some() => {}
                _ = storer.select_next_some() => {}
                res = &mut rx => return res.expect("sender not dropped"),
            }
        }
    };
    let result = tokio::time::timeout(Duration::from_secs(10), drive)
        .await
        .expect("push resolved within timeout");

    assert!(
        result.is_err(),
        "a not-responsible storer forwards; the stub forward fails and resets"
    );
    assert!(
        !reserve.contains(&address),
        "a not-responsible storer must not store the chunk"
    );
}

// --- Three-node relay (forwarding) integration tests ---
//
// These drive the real `NetworkForwarder` through the libp2p harness: node B
// relays between requester A and storer C, its outbound `ClientHandle`
// feeding back into B's own behaviour for a genuine A->B->C path. The relay
// verifies, accounts both legs, caches the forwarded chunk, and relays the
// storer's receipt verbatim.

use nectar_primitives::NetworkId;
use vertex_swarm_accounting::{Accounting, ClientAccounting, DefaultBandwidthConfig, FixedPricer};
use vertex_swarm_api::{
    Au, PeerReporter, ReportSource, SwarmBandwidthAccounting, SwarmClientAccounting,
    SwarmPeerBandwidth, SwarmPricing, SwarmScoringEvent,
};
use vertex_swarm_identity::Identity;
use vertex_swarm_spec::Spec;
use vertex_swarm_test_utils::{MockTopology, test_identity_arc};

use crate::ClientHandle;
use crate::protocol::NetworkForwarder;

/// Drops every report; the relay harness asserts on forward outcome, not the
/// scoring side effect.
struct NoopReporter;

impl PeerReporter for NoopReporter {
    fn report_peer(
        &self,
        _overlay: &OverlayAddress,
        _event: SwarmScoringEvent,
        _source: ReportSource,
    ) {
    }
}

type RelayAccounting =
    ClientAccounting<Arc<Accounting<DefaultBandwidthConfig, Arc<Identity>>>, FixedPricer<Spec>>;

fn relay_accounting() -> Arc<RelayAccounting> {
    let bandwidth = Arc::new(Accounting::new(
        DefaultBandwidthConfig::default(),
        test_identity_arc(),
    ));
    let pricer = FixedPricer::new(10_000, vertex_swarm_spec::init_mainnet());
    Arc::new(ClientAccounting::new(bandwidth, pricer))
}

/// An overlay sharing exactly `leading_bits` leading bits with `address`.
fn overlay_at_proximity(
    address: &nectar_primitives::ChunkAddress,
    leading_bits: usize,
) -> OverlayAddress {
    let mut bytes = address.0.0;
    let byte = leading_bits / 8;
    let bit = 7 - (leading_bits % 8);
    if let Some(b) = bytes.get_mut(byte) {
        *b ^= 1 << bit;
    }
    OverlayAddress::from(bytes)
}

/// Node B: a client whose forwarder relays to `storer` (via the mock
/// topology) over a `ClientHandle` wired back into B. Returns B and the
/// receiver carrying B's outbound relay commands.
fn relay_node(
    store: Arc<dyn SwarmLocalStore>,
    local: OverlayAddress,
    storer: OverlayAddress,
    accounting: Arc<RelayAccounting>,
) -> (
    Swarm<ClientBehaviour>,
    tokio::sync::mpsc::Receiver<ClientCommand>,
) {
    let (tx, rx) = tokio::sync::mpsc::channel::<ClientCommand>(16);
    let handle = ClientHandle::new(tx);
    let topology = Arc::new(MockTopology::default().with_closest(vec![storer]));
    let swarm = Swarm::new_ephemeral_tokio(move |_| {
        let mut behaviour = ClientBehaviour::new(
            Config::for_role(SwarmNodeType::Client),
            store,
            Arc::new(StubForwarder),
        );
        // Inbound receipts are recovered against this network id; the storer
        // test receipts are ground against it too.
        behaviour.set_network_id(NetworkId::MAINNET);
        let forwarder = Arc::new(NetworkForwarder::new(
            local,
            Arc::clone(&topology),
            Arc::clone(&accounting),
            handle,
            Arc::new(NoopReporter) as Arc<dyn PeerReporter>,
        ));
        behaviour.set_forwarder(forwarder);
        behaviour
    });
    (swarm, rx)
}

#[tokio::test]
async fn three_node_retrieval_relays_verifies_and_accounts() {
    let chunk = content_chunk(b"relayed through B from C");
    let address = *chunk.address();

    // A (requester) is far from the chunk; C (storer) is strictly closer; B's
    // own overlay is far too, so C is the only strictly-closer candidate.
    let a_overlay = overlay_at_proximity(&address, 2);
    let b_overlay = overlay_at_proximity(&address, 3);
    let c_overlay = overlay_at_proximity(&address, 18);

    let accounting = relay_accounting();
    let provide_price = accounting.pricing().peer_price(&a_overlay, &address);
    let receive_price = accounting.pricing().peer_price(&c_overlay, &address);

    let c_store: Arc<dyn SwarmLocalStore> =
        Arc::new(ChunkStore::with_budget(1 << 20, 1_000_000_000));
    c_store.put(chunk.clone().into()).unwrap();
    let b_store: Arc<dyn SwarmLocalStore> = Arc::new(ChunkStore::with_budget(1 << 20, 1_000));

    let mut a = swarm_with_store(Arc::new(ChunkStore::with_budget(1 << 20, 1_000)));
    let (mut b, mut b_commands) = relay_node(
        Arc::clone(&b_store),
        b_overlay,
        c_overlay,
        Arc::clone(&accounting),
    );
    let mut c = swarm_with_store(c_store);

    connect_and_activate(&mut a, &mut b, a_overlay, b_overlay).await;
    connect_and_activate(&mut b, &mut c, b_overlay, c_overlay).await;

    let (tx, mut rx) = oneshot::channel();
    a.behaviour_mut().on_command(ClientCommand::RetrieveChunk {
        peer: b_overlay,
        address,
        response: tx,
    });

    // B's forwarder commands are pumped back into B.
    let result = {
        let drive = async {
            loop {
                tokio::select! {
                    _ = a.select_next_some() => {}
                    _ = b.select_next_some() => {}
                    _ = c.select_next_some() => {}
                    Some(cmd) = b_commands.recv() => b.behaviour_mut().on_command(cmd),
                    res = &mut rx => return res.expect("sender not dropped"),
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(10), drive)
            .await
            .expect("retrieval resolved within timeout")
    };

    let delivered = result.expect("A retrieves the chunk through B");
    assert_eq!(
        delivered.chunk,
        *chunk.chunk(),
        "the chunk arrives intact at A"
    );

    // B accounted both legs: A owes B the provide price, B owes C the receive
    // price, and the forwarder earned the (positive) spread.
    assert!(
        provide_price > receive_price,
        "the forwarder earns a spread"
    );
    assert_eq!(
        accounting.bandwidth().for_peer(a_overlay).balance(),
        provide_price,
        "A is debited for the chunk B served on"
    );
    assert_eq!(
        accounting.bandwidth().for_peer(c_overlay).balance(),
        Au::ZERO - receive_price,
        "B is debited for the chunk C served it"
    );

    // The forwarded content chunk is cached stampless (the serve path strips
    // the stamp), so a later get hits.
    let cached = b_store
        .get(&address)
        .unwrap()
        .expect("the forwarded content chunk is cached at B");
    assert!(
        cached.stamp().is_none(),
        "the forwarded content chunk is cached stampless"
    );
}

#[tokio::test]
async fn relay_does_not_cache_a_forwarded_soc() {
    // A retrieved SOC arrives stampless, so it carries no version signal: the
    // relay forwards it without caching (a cached stampless SOC could later
    // serve a stale revision).
    let chunk = soc_chunk(b"feed revision", 900);
    let address = *chunk.address();

    let a_overlay = overlay_at_proximity(&address, 2);
    let b_overlay = overlay_at_proximity(&address, 3);
    let c_overlay = overlay_at_proximity(&address, 18);

    let accounting = relay_accounting();

    // A generous TTL keeps C's cached SOC fresh.
    let c_store: Arc<dyn SwarmLocalStore> = Arc::new(ChunkStore::with_budget(1 << 20, u64::MAX));
    c_store.put(chunk.clone().into()).unwrap();
    let b_store: Arc<dyn SwarmLocalStore> = Arc::new(ChunkStore::with_budget(1 << 20, u64::MAX));

    let mut a = swarm_with_store(Arc::new(ChunkStore::with_budget(1 << 20, 1_000)));
    let (mut b, mut b_commands) = relay_node(
        Arc::clone(&b_store),
        b_overlay,
        c_overlay,
        Arc::clone(&accounting),
    );
    let mut c = swarm_with_store(c_store);

    connect_and_activate(&mut a, &mut b, a_overlay, b_overlay).await;
    connect_and_activate(&mut b, &mut c, b_overlay, c_overlay).await;

    let (tx, mut rx) = oneshot::channel();
    a.behaviour_mut().on_command(ClientCommand::RetrieveChunk {
        peer: b_overlay,
        address,
        response: tx,
    });

    let result = {
        let drive = async {
            loop {
                tokio::select! {
                    _ = a.select_next_some() => {}
                    _ = b.select_next_some() => {}
                    _ = c.select_next_some() => {}
                    Some(cmd) = b_commands.recv() => b.behaviour_mut().on_command(cmd),
                    res = &mut rx => return res.expect("sender not dropped"),
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(10), drive)
            .await
            .expect("retrieval resolved within timeout")
    };

    let delivered = result.expect("A retrieves the SOC through B");
    assert_eq!(
        delivered.chunk,
        *chunk.chunk(),
        "the SOC arrives intact at A"
    );

    assert!(
        b_store.get(&address).unwrap().is_none(),
        "a forwarded SOC must not be cached"
    );
}

#[tokio::test]
async fn relay_without_strictly_closer_peer_resets_rather_than_looping() {
    // B's only candidate is no closer to the chunk than requester A, so the
    // loop bound rejects it: B cannot forward sideways or backwards, the
    // inbound retrieval resets, and no accounting reservation is taken.
    let chunk = content_chunk(b"nowhere closer to relay to");
    let address = *chunk.address();

    let a_overlay = overlay_at_proximity(&address, 12);
    let b_overlay = overlay_at_proximity(&address, 3);
    // The candidate B would forward to is farther from the chunk than A.
    let sideways = overlay_at_proximity(&address, 4);

    let accounting = relay_accounting();
    let b_store: Arc<dyn SwarmLocalStore> = Arc::new(ChunkStore::with_budget(1 << 20, 1_000));

    let mut a = swarm_with_store(Arc::new(ChunkStore::with_budget(1 << 20, 1_000)));
    let (mut b, mut b_commands) = relay_node(
        Arc::clone(&b_store),
        b_overlay,
        sideways,
        Arc::clone(&accounting),
    );

    connect_and_activate(&mut a, &mut b, a_overlay, b_overlay).await;

    let (tx, mut rx) = oneshot::channel();
    a.behaviour_mut().on_command(ClientCommand::RetrieveChunk {
        peer: b_overlay,
        address,
        response: tx,
    });

    let result = {
        let drive = async {
            loop {
                tokio::select! {
                    _ = a.select_next_some() => {}
                    _ = b.select_next_some() => {}
                    Some(cmd) = b_commands.recv() => b.behaviour_mut().on_command(cmd),
                    res = &mut rx => return res.expect("sender not dropped"),
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(10), drive)
            .await
            .expect("retrieval resolved within timeout")
    };

    assert!(
        result.is_err(),
        "a forward with no strictly-closer peer must reset, not loop"
    );
    assert_eq!(
        accounting.bandwidth().for_peer(a_overlay).balance(),
        Au::ZERO
    );
    assert_eq!(
        accounting.bandwidth().for_peer(sideways).balance(),
        Au::ZERO
    );
}

#[tokio::test]
async fn three_node_pushsync_relays_receipt_verbatim_and_accounts() {
    use alloy_signer::SignerSync;
    use alloy_signer_local::PrivateKeySigner;
    use nectar_primitives::{Nonce, compute_overlay};
    use vertex_swarm_net_pushsync::{Receipt, WireReceipt};
    use vertex_swarm_primitives::{Bin, StorageRadius};

    let chunk = content_chunk(b"pushed through B to C");
    let address = *chunk.address();

    // A (pusher) is far; B relays to the strictly-closer C, the storer of
    // record. The test answers C's outbound push with a signed receipt. B and
    // C relay that receipt verbatim (a cache-only client never signs), so A
    // sees C's exact signature, nonce, and radius. The relay seams verify
    // receipt depth, so the receipt must be genuinely deep: the signer's
    // overlay (via the nonce) must reach the declared radius for the chunk.
    let a_overlay = overlay_at_proximity(&address, 2);
    let b_overlay = overlay_at_proximity(&address, 3);
    let c_overlay = overlay_at_proximity(&address, 18);

    // The relay forwarders derive overlays with NetworkId::MAINNET, so grind
    // the nonce against that network id.
    let storer_radius = StorageRadius::new(Bin::new(7).unwrap());
    let signer = PrivateKeySigner::random();
    let signature = signer.sign_message_sync(address.as_bytes()).expect("sign");
    let mut counter = 0u64;
    let nonce = loop {
        let mut nonce_bytes = [0u8; 32];
        nonce_bytes[..8].copy_from_slice(&counter.to_le_bytes());
        let nonce = Nonce::from(nonce_bytes);
        let overlay = compute_overlay(&signer.address(), NetworkId::MAINNET, &nonce);
        if address.proximity(&overlay).get() >= storer_radius.get() {
            break nonce;
        }
        counter += 1;
    };
    let storer_receipt = WireReceipt::new(address, signature, nonce, storer_radius);
    let receipt_for_c =
        Receipt::reconstruct(storer_receipt.clone(), NetworkId::MAINNET).expect("reconstructs");

    let b_accounting = relay_accounting();
    let provide_price = b_accounting.pricing().peer_price(&a_overlay, &address);
    let receive_price = b_accounting.pricing().peer_price(&c_overlay, &address);

    let b_store: Arc<dyn SwarmLocalStore> = Arc::new(ChunkStore::with_budget(1 << 20, 1_000));
    let c_store: Arc<dyn SwarmLocalStore> = Arc::new(ChunkStore::with_budget(1 << 20, 1_000));

    let mut a = swarm_with_store(Arc::new(ChunkStore::with_budget(1 << 20, 1_000)));
    let (mut b, mut b_commands) = relay_node(
        Arc::clone(&b_store),
        b_overlay,
        c_overlay,
        Arc::clone(&b_accounting),
    );
    // C relays to a notional deeper node; the test answers C's outbound push
    // command directly with the signed receipt, so C is the effective storer.
    let deeper = overlay_at_proximity(&address, 24);
    let c_accounting = relay_accounting();
    let (mut c, mut c_commands) = relay_node(
        Arc::clone(&c_store),
        c_overlay,
        deeper,
        Arc::clone(&c_accounting),
    );

    connect_and_activate(&mut a, &mut b, a_overlay, b_overlay).await;
    connect_and_activate(&mut b, &mut c, b_overlay, c_overlay).await;

    let (tx, mut rx) = oneshot::channel();
    a.behaviour_mut().on_command(ClientCommand::PushChunk {
        peer: b_overlay,
        address,
        chunk,
        response: tx,
    });

    let result = {
        let drive = async {
            loop {
                tokio::select! {
                    _ = a.select_next_some() => {}
                    _ = b.select_next_some() => {}
                    _ = c.select_next_some() => {}
                    Some(cmd) = b_commands.recv() => b.behaviour_mut().on_command(cmd),
                    // C is the storer: answer its outbound push with the
                    // signed receipt instead of forwarding on.
                    Some(cmd) = c_commands.recv() => {
                        if let ClientCommand::PushChunk { response, .. } = cmd {
                            let _ = response.send(Ok(receipt_for_c.clone()));
                        }
                    }
                    res = &mut rx => return res.expect("sender not dropped"),
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(10), drive)
            .await
            .expect("push resolved within timeout")
    };

    let relayed = result.expect("A receives the storer's receipt through B");
    // Verbatim across both hops: A sees C's exact wire receipt and storer,
    // never a re-signed value.
    assert_eq!(relayed.to_wire(), storer_receipt);
    assert_eq!(relayed.storer, receipt_for_c.storer);

    // B accounted both legs of the relay.
    assert!(
        provide_price > receive_price,
        "the forwarder earns a spread"
    );
    assert_eq!(
        b_accounting.bandwidth().for_peer(a_overlay).balance(),
        provide_price
    );
    assert_eq!(
        b_accounting.bandwidth().for_peer(c_overlay).balance(),
        Au::ZERO - receive_price
    );
}
