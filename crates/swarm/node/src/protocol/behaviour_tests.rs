//! Behaviour-level integration tests for the re-exported [`ClientBehaviour`]
//! driven through the node-local [`NetworkForwarder`].
//!
//! These exercise the real libp2p handler over the swarm-test harness: cache
//! serving, the stub-forwarder reset paths, storer ingest (store and sign), and
//! the three-node relay that needs the concrete forwarder. They live in the node
//! crate because the relay tests construct a `NetworkForwarder` (which couples to
//! accounting and the outbound `ClientHandle`) over the behaviour the
//! `vertex-swarm-client-behaviour` crate provides.

use std::sync::Arc;
use std::time::Duration;

use alloy_primitives::{B256, Signature};
use alloy_signer_local::PrivateKeySigner;
use libp2p::{PeerId, Swarm};
use nectar_postage::Stamp;
use nectar_primitives::{AnyChunk, ContentChunk, SingleOwnerChunk, XorMetric};
use tokio::sync::oneshot;
use vertex_swarm_api::{StorageRadius, SwarmLocalStore};
use vertex_swarm_localstore::{ChunkStore, Clock};
use vertex_swarm_primitives::{OverlayAddress, StampedChunk, SwarmNodeType};
use vertex_swarm_test_utils::MockReserve;
use vertex_swarm_test_utils::harness::{
    CommandPump, DrivableSwarm, HarnessNode, NodeContext, connect_and_activate, drive_until_signal,
    seeded_identity,
};

use crate::ChunkTransferError;
use crate::client_service::RetrievalResult;
use crate::protocol::{
    BehaviourConfig as Config, ClientBehaviour, ClientCommand, PeerCommand, StubForwarder,
};

/// Fixed-instant clock for SOC freshness tests.
struct FixedClock(i64);

impl Clock for FixedClock {
    fn now_ns(&self) -> i64 {
        self.0
    }
}

fn content_chunk(payload: &'static [u8]) -> StampedChunk {
    let sig = Signature::from_raw(&[1u8; 65]).expect("valid signature");
    let stamp = Stamp::new(B256::repeat_byte(0xaa).into(), 3, 7, 42, sig);
    let chunk: AnyChunk = ContentChunk::new(payload)
        .expect("valid content chunk")
        .into();
    StampedChunk::new(chunk, stamp)
}

fn soc_chunk(payload: &'static [u8], stamp_ns: u64) -> StampedChunk {
    let sig = Signature::from_raw(&[1u8; 65]).expect("valid signature");
    let stamp = Stamp::new(B256::repeat_byte(0xaa).into(), 3, 7, stamp_ns, sig);
    let signer = PrivateKeySigner::from_bytes(&B256::repeat_byte(0x11)).expect("signer");
    let chunk: AnyChunk = SingleOwnerChunk::new(B256::repeat_byte(0x22).into(), payload, &signer)
        .expect("valid soc")
        .into();
    StampedChunk::new(chunk, stamp)
}

fn overlay(n: u8) -> OverlayAddress {
    OverlayAddress::from([n; 32])
}

/// Build a client-shaped harness node with a caller-chosen overlay.
///
/// The seed fixes the libp2p identity; the overlay is set explicitly so
/// proximity-sensitive relay tests can place a node while the harness still owns
/// the connection registry the behaviour reads through `ctx.identities`.
fn node_with_overlay(
    seed: u8,
    overlay: OverlayAddress,
    build: impl FnOnce(&NodeContext) -> ClientBehaviour,
) -> HarnessNode<ClientBehaviour> {
    let mut id = seeded_identity(seed, SwarmNodeType::Client);
    id.overlay = overlay;
    HarnessNode::new(&id, build)
}

/// A cache-only client node carrying `overlay` and serving from `store`.
fn client_node(
    seed: u8,
    overlay: OverlayAddress,
    store: Arc<dyn SwarmLocalStore>,
) -> HarnessNode<ClientBehaviour> {
    node_with_overlay(seed, overlay, move |ctx| {
        ClientBehaviour::new(
            Config::for_role(SwarmNodeType::Client),
            store,
            Arc::new(StubForwarder),
            ctx.identities.clone(),
        )
    })
}

/// Activation hook for [`connect_and_activate`]: promote the remote peer to
/// Active in the behaviour so its request/serve path goes live.
fn activate_client(
    behaviour: &mut ClientBehaviour,
    peer_id: PeerId,
    overlay: OverlayAddress,
    node_type: SwarmNodeType,
) {
    behaviour.on_command(ClientCommand::ActivatePeer {
        peer_id,
        overlay,
        node_type,
    });
}

async fn drive_until_retrieved(
    client: &mut Swarm<ClientBehaviour>,
    server: &mut Swarm<ClientBehaviour>,
    mut rx: oneshot::Receiver<Result<RetrievalResult, ChunkTransferError>>,
) -> Result<RetrievalResult, ChunkTransferError> {
    let mut drivables: [&mut dyn DrivableSwarm; 2] = [client, server];
    drive_until_signal(&mut drivables, Duration::from_secs(10), &mut rx)
        .await
        .expect("retrieval resolved within timeout")
}

#[tokio::test(start_paused = true)]
async fn serves_a_content_chunk_from_the_cache() {
    let chunk = content_chunk(b"served from cache");
    let address = *chunk.address();

    let server_store: Arc<dyn SwarmLocalStore> =
        Arc::new(ChunkStore::with_budget(1 << 20, 1_000_000_000));
    server_store.put(chunk.clone().into()).unwrap();

    let server_overlay = overlay(2);
    let mut client = client_node(
        1,
        overlay(1),
        Arc::new(ChunkStore::with_budget(1 << 20, 1_000)),
    );
    let mut server = client_node(2, server_overlay, server_store);

    connect_and_activate(&mut client, &mut server, activate_client).await;

    let (tx, rx) = oneshot::channel();
    client
        .swarm
        .behaviour_mut()
        .on_command(ClientCommand::Peer {
            peer: server_overlay,
            command: PeerCommand::RetrieveChunk {
                address,
                response: tx,
                originated: true,
            },
        });

    let result = drive_until_retrieved(&mut client.swarm, &mut server.swarm, rx).await;
    let delivered = result.expect("served from cache");
    assert_eq!(*delivered.chunk.address(), address);
    assert_eq!(delivered.chunk, *chunk.chunk());
}

#[tokio::test(start_paused = true)]
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

    let server_overlay = overlay(2);
    let mut client = client_node(
        1,
        overlay(1),
        Arc::new(ChunkStore::with_budget(1 << 20, 1_000)),
    );
    let mut server = client_node(2, server_overlay, server_store);

    connect_and_activate(&mut client, &mut server, activate_client).await;

    let (tx, rx) = oneshot::channel();
    client
        .swarm
        .behaviour_mut()
        .on_command(ClientCommand::Peer {
            peer: server_overlay,
            command: PeerCommand::RetrieveChunk {
                address,
                response: tx,
                originated: true,
            },
        });

    let delivered = drive_until_retrieved(&mut client.swarm, &mut server.swarm, rx)
        .await
        .expect("fresh SOC served from cache");
    assert_eq!(delivered.chunk, *chunk.chunk());
}

#[tokio::test(start_paused = true)]
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

    let server_overlay = overlay(2);
    let mut client = client_node(
        1,
        overlay(1),
        Arc::new(ChunkStore::with_budget(1 << 20, 1_000)),
    );
    let mut server = client_node(2, server_overlay, server_store);

    connect_and_activate(&mut client, &mut server, activate_client).await;

    let (tx, rx) = oneshot::channel();
    client
        .swarm
        .behaviour_mut()
        .on_command(ClientCommand::Peer {
            peer: server_overlay,
            command: PeerCommand::RetrieveChunk {
                address,
                response: tx,
                originated: true,
            },
        });

    let result = drive_until_retrieved(&mut client.swarm, &mut server.swarm, rx).await;
    assert!(
        result.is_err(),
        "an expired SOC must not be served; the stream resets so the requester forwards"
    );
}

#[tokio::test(start_paused = true)]
async fn cache_miss_resets_with_stub_forwarder() {
    // Empty cache plus stub forwarder: the inbound retrieval can neither
    // serve nor forward, so the substream resets and the requester fails.
    let address = *content_chunk(b"never cached").address();

    let server_overlay = overlay(2);
    let mut client = client_node(
        1,
        overlay(1),
        Arc::new(ChunkStore::with_budget(1 << 20, 1_000)),
    );
    let mut server = client_node(
        2,
        server_overlay,
        Arc::new(ChunkStore::with_budget(1 << 20, 1_000)),
    );

    connect_and_activate(&mut client, &mut server, activate_client).await;

    let (tx, rx) = oneshot::channel();
    client
        .swarm
        .behaviour_mut()
        .on_command(ClientCommand::Peer {
            peer: server_overlay,
            command: PeerCommand::RetrieveChunk {
                address,
                response: tx,
                originated: true,
            },
        });

    let result = drive_until_retrieved(&mut client.swarm, &mut server.swarm, rx).await;
    assert!(
        result.is_err(),
        "a cache miss with the stub forwarder must reset the stream"
    );
}

#[tokio::test(start_paused = true)]
async fn inbound_pushsync_resets_with_stub_forwarder() {
    // A cache-only client never takes custody: inbound pushsync forwards, the
    // stub forward fails, the substream resets, and no receipt is signed.
    let chunk = content_chunk(b"pushed chunk");

    let server_overlay = overlay(2);
    let mut client = client_node(
        1,
        overlay(1),
        Arc::new(ChunkStore::with_budget(1 << 20, 1_000)),
    );
    let mut server = client_node(
        2,
        server_overlay,
        Arc::new(ChunkStore::with_budget(1 << 20, 1_000)),
    );

    connect_and_activate(&mut client, &mut server, activate_client).await;

    let (tx, mut rx) = oneshot::channel();
    client
        .swarm
        .behaviour_mut()
        .on_command(ClientCommand::Peer {
            peer: server_overlay,
            command: PeerCommand::PushChunk {
                chunk,
                response: tx,
                originated: true,
            },
        });

    let mut drivables: [&mut dyn DrivableSwarm; 2] = [&mut client, &mut server];
    let result = drive_until_signal(&mut drivables, Duration::from_secs(10), &mut rx)
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
// through the real libp2p handler. The reserve is the canonical `MockReserve`
// mutable point store: a fixed responsibility flag pins either branch.

/// Storer harness node holding the ingest capability. Returns the node, the
/// shared reserve (to assert what was stored), the signer and nonce (to assert
/// the receipt recovers to the storer's overlay).
///
/// The node presents as `Client` to the connection registry, matching how a
/// pusher labels a serving peer; the storer role comes from the behaviour's
/// `for_role(Storer)` config and its installed capability, not the registry
/// label.
fn storer_node(
    seed: u8,
    overlay: OverlayAddress,
    responsible: bool,
    radius: StorageRadius,
) -> (
    HarnessNode<ClientBehaviour>,
    Arc<MockReserve>,
    PrivateKeySigner,
    vertex_swarm_primitives::Nonce,
) {
    use nectar_primitives::NetworkId;
    use vertex_swarm_identity::Identity;
    use vertex_swarm_primitives::Nonce;
    use vertex_swarm_spec::SpecBuilder;

    let reserve = Arc::new(MockReserve::new(responsible, radius));
    let signer = PrivateKeySigner::random();
    let nonce = Nonce::from([0x5a; 32]);

    let reserve_for_node = Arc::clone(&reserve);
    let signer_for_node = signer.clone();
    let node = node_with_overlay(seed, overlay, move |ctx| {
        // The reserve serves on retrieval too, so it is the behaviour's store.
        let store: Arc<dyn SwarmLocalStore> = Arc::clone(&reserve_for_node) as _;
        let mut behaviour = ClientBehaviour::new(
            Config::for_role(SwarmNodeType::Storer),
            store,
            Arc::new(StubForwarder),
            ctx.identities.clone(),
        );
        behaviour.set_network_id(NetworkId::MAINNET);
        let spec = Arc::new(
            SpecBuilder::mainnet()
                .network_id(NetworkId::MAINNET.get())
                .build(),
        );
        let identity = Identity::new(signer_for_node.clone(), nonce, spec, SwarmNodeType::Storer);
        let capability = crate::protocol::StorerCapability::new(
            Arc::clone(&reserve_for_node) as Arc<dyn vertex_swarm_api::ReserveStore>,
            Arc::new(identity) as Arc<dyn vertex_swarm_primitives::OverlaySigner + Send + Sync>,
        );
        behaviour.set_storer(capability);
        behaviour
    });
    (node, reserve, signer, nonce)
}

#[tokio::test(start_paused = true)]
async fn responsible_storer_stores_and_signs_a_receipt() {
    use nectar_primitives::{NetworkId, compute_overlay};
    use vertex_swarm_primitives::Bin;

    let chunk = content_chunk(b"stored by the responsible storer");
    let address = *chunk.address();
    let radius = StorageRadius::new(Bin::new(4).unwrap());

    let storer_overlay = overlay(2);
    let (mut storer, reserve, signer, nonce) = storer_node(2, storer_overlay, true, radius);
    let mut pusher = client_node(
        1,
        overlay(1),
        Arc::new(ChunkStore::with_budget(1 << 20, 1_000)),
    );

    connect_and_activate(&mut pusher, &mut storer, activate_client).await;

    let (tx, mut rx) = oneshot::channel();
    pusher
        .swarm
        .behaviour_mut()
        .on_command(ClientCommand::Peer {
            peer: storer_overlay,
            command: PeerCommand::PushChunk {
                chunk,
                response: tx,
                originated: true,
            },
        });

    let mut drivables: [&mut dyn DrivableSwarm; 2] = [&mut pusher, &mut storer];
    let result = drive_until_signal(&mut drivables, Duration::from_secs(10), &mut rx)
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

#[tokio::test(start_paused = true)]
async fn non_responsible_storer_forwards_instead_of_storing() {
    use vertex_swarm_primitives::Bin;

    // The storer holds the ingest capability but is NOT responsible, so it
    // forwards; the stub forward fails, the substream resets, nothing is
    // stored, and no receipt is signed.
    let chunk = content_chunk(b"not my responsibility");
    let address = *chunk.address();
    let radius = StorageRadius::new(Bin::new(4).unwrap());

    let storer_overlay = overlay(2);
    let (mut storer, reserve, _signer, _nonce) = storer_node(2, storer_overlay, false, radius);
    let mut pusher = client_node(
        1,
        overlay(1),
        Arc::new(ChunkStore::with_budget(1 << 20, 1_000)),
    );

    connect_and_activate(&mut pusher, &mut storer, activate_client).await;

    let (tx, mut rx) = oneshot::channel();
    pusher
        .swarm
        .behaviour_mut()
        .on_command(ClientCommand::Peer {
            peer: storer_overlay,
            command: PeerCommand::PushChunk {
                chunk,
                response: tx,
                originated: true,
            },
        });

    let mut drivables: [&mut dyn DrivableSwarm; 2] = [&mut pusher, &mut storer];
    let result = drive_until_signal(&mut drivables, Duration::from_secs(10), &mut rx)
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
use vertex_swarm_accounting::{Accounting, ClientAccounting, DefaultAccountingConfig, FixedPricer};
use vertex_swarm_api::{
    Au, SwarmAccounting, SwarmClientAccounting, SwarmPeerAccounting, SwarmPricing,
};
use vertex_swarm_identity::Identity;
use vertex_swarm_spec::Spec;
use vertex_swarm_test_utils::{MockTopology, test_identity_arc};

use crate::ClientHandle;
use crate::protocol::NetworkForwarder;

/// Drops every settle drive; relay walks never settle.
struct NoTriggeredSettle;

impl crate::SettlementTrigger for NoTriggeredSettle {
    fn trigger_settlement(&self, _peer: OverlayAddress) {}
}

type RelayAccounting =
    ClientAccounting<Arc<Accounting<DefaultAccountingConfig, Arc<Identity>>>, FixedPricer<Spec>>;

fn relay_accounting() -> Arc<RelayAccounting> {
    let bandwidth = Arc::new(Accounting::new(
        DefaultAccountingConfig::default(),
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
    let mut bytes = <[u8; 32]>::from(*address);
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
    seed: u8,
    store: Arc<dyn SwarmLocalStore>,
    local: OverlayAddress,
    storer: OverlayAddress,
    accounting: Arc<RelayAccounting>,
) -> (
    HarnessNode<ClientBehaviour>,
    tokio::sync::mpsc::Receiver<ClientCommand>,
) {
    let (tx, rx) = tokio::sync::mpsc::channel::<ClientCommand>(16);
    let handle = ClientHandle::new(tx);
    let topology = MockTopology::default()
        .with_closest(vec![storer])
        .with_overlay(local);
    let engine = crate::DispatchEngine::new(
        handle,
        Arc::new(topology) as Arc<dyn crate::RetrievalTopology>,
        vertex_swarm_api::Bin::new(31).unwrap(),
        crate::ProximityOnly,
        Arc::new(crate::PeerInflightLimiter::new(
            std::num::NonZeroUsize::new(4).unwrap(),
        )),
        crate::NoLatencyHint,
        Arc::new(NoTriggeredSettle),
    );
    let node = node_with_overlay(seed, local, move |ctx| {
        let mut behaviour = ClientBehaviour::new(
            Config::for_role(SwarmNodeType::Client),
            store,
            Arc::new(StubForwarder),
            ctx.identities.clone(),
        );
        // Inbound receipts are recovered against this network id; the storer
        // test receipts are ground against it too.
        behaviour.set_network_id(NetworkId::MAINNET);
        let forwarder = Arc::new(NetworkForwarder::new(
            engine.clone(),
            Arc::clone(&accounting),
        ));
        behaviour.set_forwarder(forwarder);
        behaviour
    });
    (node, rx)
}

#[tokio::test(start_paused = true)]
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

    let mut a = client_node(
        1,
        a_overlay,
        Arc::new(ChunkStore::with_budget(1 << 20, 1_000)),
    );
    let (mut b, mut b_commands) = relay_node(
        2,
        Arc::clone(&b_store),
        b_overlay,
        c_overlay,
        Arc::clone(&accounting),
    );
    let mut c = client_node(3, c_overlay, c_store);

    connect_and_activate(&mut a, &mut b, activate_client).await;
    connect_and_activate(&mut b, &mut c, activate_client).await;

    let (tx, mut rx) = oneshot::channel();
    a.swarm.behaviour_mut().on_command(ClientCommand::Peer {
        peer: b_overlay,
        command: PeerCommand::RetrieveChunk {
            address,
            response: tx,
            originated: true,
        },
    });

    // B's forwarder commands are pumped back into B.
    let result = {
        let mut b_pump = CommandPump::new(&mut b.swarm, &mut b_commands, |behaviour, command| {
            behaviour.on_command(command);
        });
        let mut drivables: [&mut dyn DrivableSwarm; 3] = [&mut a, &mut b_pump, &mut c];
        drive_until_signal(&mut drivables, Duration::from_secs(10), &mut rx)
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
        accounting.accounting().for_peer(a_overlay).balance(),
        provide_price,
        "A is debited for the chunk B served on"
    );
    assert_eq!(
        accounting.accounting().for_peer(c_overlay).balance(),
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

#[tokio::test(start_paused = true)]
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

    let mut a = client_node(
        1,
        a_overlay,
        Arc::new(ChunkStore::with_budget(1 << 20, 1_000)),
    );
    let (mut b, mut b_commands) = relay_node(
        2,
        Arc::clone(&b_store),
        b_overlay,
        c_overlay,
        Arc::clone(&accounting),
    );
    let mut c = client_node(3, c_overlay, c_store);

    connect_and_activate(&mut a, &mut b, activate_client).await;
    connect_and_activate(&mut b, &mut c, activate_client).await;

    let (tx, mut rx) = oneshot::channel();
    a.swarm.behaviour_mut().on_command(ClientCommand::Peer {
        peer: b_overlay,
        command: PeerCommand::RetrieveChunk {
            address,
            response: tx,
            originated: true,
        },
    });

    let result = {
        let mut b_pump = CommandPump::new(&mut b.swarm, &mut b_commands, |behaviour, command| {
            behaviour.on_command(command);
        });
        let mut drivables: [&mut dyn DrivableSwarm; 3] = [&mut a, &mut b_pump, &mut c];
        drive_until_signal(&mut drivables, Duration::from_secs(10), &mut rx)
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

#[tokio::test(start_paused = true)]
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

    let mut a = client_node(
        1,
        a_overlay,
        Arc::new(ChunkStore::with_budget(1 << 20, 1_000)),
    );
    let (mut b, mut b_commands) = relay_node(
        2,
        Arc::clone(&b_store),
        b_overlay,
        sideways,
        Arc::clone(&accounting),
    );

    connect_and_activate(&mut a, &mut b, activate_client).await;

    let (tx, mut rx) = oneshot::channel();
    a.swarm.behaviour_mut().on_command(ClientCommand::Peer {
        peer: b_overlay,
        command: PeerCommand::RetrieveChunk {
            address,
            response: tx,
            originated: true,
        },
    });

    let result = {
        let mut b_pump = CommandPump::new(&mut b.swarm, &mut b_commands, |behaviour, command| {
            behaviour.on_command(command);
        });
        let mut drivables: [&mut dyn DrivableSwarm; 2] = [&mut a, &mut b_pump];
        drive_until_signal(&mut drivables, Duration::from_secs(10), &mut rx)
            .await
            .expect("retrieval resolved within timeout")
    };

    assert!(
        result.is_err(),
        "a forward with no strictly-closer peer must reset, not loop"
    );
    assert_eq!(
        accounting.accounting().for_peer(a_overlay).balance(),
        Au::ZERO
    );
    assert_eq!(
        accounting.accounting().for_peer(sideways).balance(),
        Au::ZERO
    );
}

#[tokio::test(start_paused = true)]
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

    let mut a = client_node(
        1,
        a_overlay,
        Arc::new(ChunkStore::with_budget(1 << 20, 1_000)),
    );
    let (mut b, mut b_commands) = relay_node(
        2,
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
        3,
        Arc::clone(&c_store),
        c_overlay,
        deeper,
        Arc::clone(&c_accounting),
    );

    connect_and_activate(&mut a, &mut b, activate_client).await;
    connect_and_activate(&mut b, &mut c, activate_client).await;

    let (tx, mut rx) = oneshot::channel();
    a.swarm.behaviour_mut().on_command(ClientCommand::Peer {
        peer: b_overlay,
        command: PeerCommand::PushChunk {
            chunk,
            response: tx,
            originated: true,
        },
    });

    let result = {
        let mut b_pump = CommandPump::new(&mut b.swarm, &mut b_commands, |behaviour, command| {
            behaviour.on_command(command);
        });
        // C is the storer: answer its outbound push with the signed receipt
        // instead of forwarding on.
        let mut c_pump = CommandPump::new(&mut c.swarm, &mut c_commands, |_behaviour, command| {
            if let ClientCommand::Peer {
                command: PeerCommand::PushChunk { response, .. },
                ..
            } = command
            {
                let _ = response.send(Ok(receipt_for_c.clone()));
            }
        });
        let mut drivables: [&mut dyn DrivableSwarm; 3] = [&mut a, &mut b_pump, &mut c_pump];
        drive_until_signal(&mut drivables, Duration::from_secs(10), &mut rx)
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
        b_accounting.accounting().for_peer(a_overlay).balance(),
        provide_price
    );
    assert_eq!(
        b_accounting.accounting().for_peer(c_overlay).balance(),
        Au::ZERO - receive_price
    );
}
