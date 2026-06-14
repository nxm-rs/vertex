//! Unified client for Swarm nodes.

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use futures::{FutureExt, StreamExt, stream::FuturesUnordered};
use futures_timer::Delay;
use nectar_primitives::{AnyChunk, ChunkAddress};
use vertex_swarm_api::{
    BootnodeComponents, ClientComponents, HasAccounting, HasTopology, StampedChunk, SwarmClient,
    SwarmClientAccounting, SwarmError, SwarmResult, SwarmTopologyRouting,
};
use vertex_swarm_primitives::OverlayAddress;

use crate::{ClientHandle, PeerSelector};

/// Number of closest peers raced for a retrieval (and walked for a push).
const CLOSEST_PEER_COUNT: usize = 3;

/// Delay before each additional retrieval candidate joins the race.
///
/// Staggering bounds the cost of the fan-out: every raced attempt the remote
/// answers is paid for in accounting units, so further candidates only start
/// while no response has arrived. A failed attempt starts the next candidate
/// immediately instead of waiting out the stagger.
const RETRIEVAL_STAGGER: Duration = Duration::from_millis(500);

/// Unified client for all Swarm node types.
///
/// Generic over component type `C`:
/// - [`BootnodeComponents<T>`] for bootnodes (topology only)
/// - [`ClientComponents<T, A>`] for client/storer nodes (topology + accounting)
pub struct Client<C, S = ()> {
    components: C,
    client_handle: ClientHandle,
    selector: Option<Arc<PeerSelector>>,
    _storage: std::marker::PhantomData<S>,
}

impl<C, S> Client<C, S> {
    /// Create a client from components.
    pub fn new(components: C, client_handle: ClientHandle) -> Self {
        Self {
            components,
            client_handle,
            selector: None,
            _storage: std::marker::PhantomData,
        }
    }

    /// Order retrieval and pushsync candidates with `selector` (score- and
    /// affordability-aware) instead of plain proximity order.
    pub fn with_selector(mut self, selector: Arc<PeerSelector>) -> Self {
        self.selector = Some(selector);
        self
    }

    /// Order proximity-sorted `candidates` for a request on `chunk`.
    fn select(&self, candidates: Vec<OverlayAddress>, chunk: &ChunkAddress) -> Vec<OverlayAddress> {
        match &self.selector {
            Some(selector) => selector.order(candidates, chunk),
            None => candidates,
        }
    }

    /// Get the client handle.
    pub fn client_handle(&self) -> &ClientHandle {
        &self.client_handle
    }

    /// Get the components.
    pub fn components(&self) -> &C {
        &self.components
    }
}

impl<C: HasTopology, S> Client<C, S> {
    /// Get the topology.
    pub fn topology(&self) -> &C::Topology {
        self.components.topology()
    }
}

impl<C: HasAccounting, S> Client<C, S> {
    /// Get the accounting.
    pub fn accounting(&self) -> &C::Accounting {
        self.components.accounting()
    }
}

// Bootnode constructors
impl<T> Client<BootnodeComponents<T>, ()> {
    /// Create a bootnode client (topology only).
    pub fn bootnode(topology: T, client_handle: ClientHandle) -> Self {
        Self::new(BootnodeComponents::new(topology), client_handle)
    }
}

// Client constructors
impl<T, A> Client<ClientComponents<T, A>, ()> {
    /// Create a client node (topology + accounting).
    #[allow(clippy::self_named_constructors)]
    pub fn client(topology: T, accounting: A, client_handle: ClientHandle) -> Self {
        Self::new(ClientComponents::new(topology, accounting), client_handle)
    }
}

#[async_trait]
impl<T, A, S> SwarmClient for Client<ClientComponents<T, A>, S>
where
    T: SwarmTopologyRouting + Send + Sync + 'static,
    A: SwarmClientAccounting + Send + Sync + 'static,
    S: Send + Sync + 'static,
{
    async fn get(&self, address: &ChunkAddress) -> SwarmResult<AnyChunk> {
        let closest = self
            .components
            .topology()
            .closest_to(address, CLOSEST_PEER_COUNT);
        let closest = self.select(closest, address);
        let attempts = closest.len();
        let mut candidates = closest.into_iter();

        // Race the candidates with a staggered start: the best candidate is
        // queried immediately and each stagger tick (or failed attempt) adds
        // the next one, resolving on the first response. The codec
        // reconstructs the chunk against the requested address, so the
        // retrieved chunk is already address-validated; no re-verification is
        // needed here. Each request carries its own response channel, so
        // concurrent requests for one address never collide, and losing
        // attempts are simply dropped when this future returns.
        let mut in_flight = FuturesUnordered::new();
        match candidates.next() {
            Some(peer) => in_flight.push(self.client_handle.retrieve_chunk(peer, *address)),
            None => {
                return Err(SwarmError::NoStorer {
                    chunk_address: *address,
                });
            }
        }

        let mut stagger = Delay::new(RETRIEVAL_STAGGER).fuse();

        loop {
            futures::select! {
                result = in_flight.select_next_some() => match result {
                    Ok(result) => return Ok(result.chunk),
                    Err(error) => {
                        // A failed attempt frees its slot: start the next
                        // candidate immediately. Once no candidates and no
                        // attempts remain, the race ends with the error of
                        // the last attempt to fail.
                        if let Some(peer) = candidates.next() {
                            in_flight.push(self.client_handle.retrieve_chunk(peer, *address));
                        } else if in_flight.is_empty() {
                            return Err(SwarmError::AllPeersFailed {
                                address: *address,
                                attempts,
                                source: Box::new(error),
                            });
                        }
                    }
                },
                _ = stagger => {
                    if let Some(peer) = candidates.next() {
                        in_flight.push(self.client_handle.retrieve_chunk(peer, *address));
                        stagger = Delay::new(RETRIEVAL_STAGGER).fuse();
                    }
                }
            }
        }
    }

    async fn put(&self, chunk: StampedChunk) -> SwarmResult<()> {
        let address = *chunk.address();
        let closest = self
            .components
            .topology()
            .closest_to(&address, CLOSEST_PEER_COUNT);
        let closest = self.select(closest, &address);
        let attempts = closest.len();

        // Walk the candidates in order. Pushes are not raced: a race would
        // deliver and charge for the chunk on every branch. Failures resolve
        // promptly through the per-request response channel, so a dead
        // candidate cannot stall the walk.
        //
        // The seed error covers the no-candidates case; each failed attempt
        // replaces it, so the value after the loop is always the last failure.
        let mut outcome = Err(SwarmError::NoStorer {
            chunk_address: address,
        });
        for peer in closest {
            match self.client_handle.push_chunk(peer, chunk.clone()).await {
                Ok(_receipt) => return Ok(()),
                Err(error) => {
                    outcome = Err(SwarmError::AllPeersFailed {
                        address,
                        attempts,
                        source: Box::new(error),
                    });
                }
            }
        }

        outcome
    }
}

/// Bootnode client (topology only).
pub type BootnodeClient<T> = Client<BootnodeComponents<T>, ()>;

/// Full client (topology + accounting).
pub type FullClient<T, A, S = ()> = Client<ClientComponents<T, A>, S>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClientCommand, ClientHandle};
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use vertex_swarm_api::{SwarmBandwidthAccounting, SwarmTopologyRouting};
    use vertex_swarm_bandwidth::{Accounting, FixedPricer};
    use vertex_swarm_bandwidth::{ClientAccounting, DefaultBandwidthConfig};
    use vertex_swarm_test_utils::{MockTopology, test_identity_arc as test_identity};

    fn create_test_handle() -> ClientHandle {
        let (tx, _rx) = mpsc::channel::<ClientCommand>(16);
        ClientHandle::new(tx)
    }

    #[test]
    fn test_bootnode_client() {
        let topology = MockTopology::default();
        let handle = create_test_handle();

        let client = Client::bootnode(topology, handle);
        let _ = client
            .topology()
            .neighbors(vertex_swarm_primitives::NeighborhoodDepth::ZERO);
    }

    #[test]
    fn test_full_client() {
        let topology = MockTopology::default();
        let bandwidth = Arc::new(Accounting::new(
            DefaultBandwidthConfig::default(),
            test_identity(),
        ));
        let pricer = FixedPricer::new(10_000, vertex_swarm_spec::init_mainnet());
        let accounting = ClientAccounting::new(bandwidth, pricer);
        let handle = create_test_handle();

        let client: FullClient<MockTopology, ClientAccounting<_, _>> =
            Client::client(topology, accounting, handle);

        let peers = SwarmBandwidthAccounting::peers(client.accounting().bandwidth());
        assert!(peers.is_empty());
    }

    use crate::ChunkTransferError;
    use alloy_primitives::{B256, Signature};
    use alloy_signer::SignerSync;
    use alloy_signer_local::PrivateKeySigner;
    use nectar_primitives::{ContentChunk, NetworkId, Nonce, compute_overlay};
    use vertex_swarm_api::Stamp;
    use vertex_swarm_net_pushsync::{Receipt, WireReceipt};
    use vertex_swarm_primitives::{Bin, OverlayAddress, StorageRadius};

    fn test_peer() -> OverlayAddress {
        OverlayAddress::from([7u8; 32])
    }

    fn test_signature() -> Signature {
        let mut raw = [0u8; 65];
        raw[..64].fill(1);
        raw[64] = 27;
        Signature::try_from(&raw[..]).expect("valid signature bytes")
    }

    /// A storer-verified receipt over `address`, as the decode boundary produces
    /// it, so a test can resolve a push command with a real `Receipt`.
    fn signed_receipt(address: &ChunkAddress) -> (Receipt, OverlayAddress) {
        let signer = PrivateKeySigner::random();
        let signature = signer.sign_message_sync(address.as_bytes()).expect("sign");
        let nonce = Nonce::from([9u8; 32]);
        let overlay = compute_overlay(&signer.address(), NetworkId::MAINNET, &nonce);
        let wire = WireReceipt::new(
            *address,
            signature,
            nonce,
            StorageRadius::new(Bin::new(5).unwrap()),
        );
        (
            Receipt::reconstruct(wire, NetworkId::MAINNET).expect("reconstructs"),
            overlay,
        )
    }

    fn test_stamp() -> Stamp {
        Stamp::new(B256::repeat_byte(0xaa), 3, 7, 42, test_signature())
    }

    fn test_chunk() -> nectar_primitives::AnyChunk {
        ContentChunk::new(&b"chunk-bytes"[..])
            .expect("valid content chunk")
            .into()
    }

    fn test_stamped_chunk() -> StampedChunk {
        StampedChunk::new(test_chunk(), test_stamp())
    }

    fn test_address() -> ChunkAddress {
        *test_stamped_chunk().address()
    }

    fn client_with_topology(topology: MockTopology) -> impl SwarmClient {
        let bandwidth = Arc::new(Accounting::new(
            DefaultBandwidthConfig::default(),
            test_identity(),
        ));
        let pricer = FixedPricer::new(10_000, vertex_swarm_spec::init_mainnet());
        let accounting = ClientAccounting::new(bandwidth, pricer);
        Client::client(topology, accounting, create_test_handle())
    }

    #[tokio::test]
    async fn get_fails_with_no_storer_without_candidates() {
        let client = client_with_topology(MockTopology::default());

        let err = client.get(&test_address()).await.unwrap_err();
        assert!(matches!(err, SwarmError::NoStorer { .. }));
    }

    #[tokio::test]
    async fn get_fails_with_all_peers_failed_when_every_peer_errors() {
        let closest = vec![
            OverlayAddress::from([1u8; 32]),
            OverlayAddress::from([2u8; 32]),
        ];
        // The test handle's command channel is closed, so every retrieval
        // attempt fails and the client must surface the all-peers-failed path.
        let client = client_with_topology(MockTopology::default().with_closest(closest));

        let err = client.get(&test_address()).await.unwrap_err();
        assert!(matches!(
            err,
            SwarmError::AllPeersFailed { attempts: 2, .. }
        ));
    }

    #[tokio::test]
    async fn push_chunk_emits_command_and_resolves_on_receipt() {
        let (tx, mut rx) = mpsc::channel::<ClientCommand>(16);
        let handle = ClientHandle::new(tx);

        let peer = test_peer();
        let stamped = test_stamped_chunk();
        let address = *stamped.address();

        let push = {
            let handle = handle.clone();
            tokio::spawn(async move { handle.push_chunk(peer, stamped).await })
        };

        // The handle must have emitted a PushChunk command for the target peer.
        let cmd = rx.recv().await.expect("command emitted");
        let response = match cmd {
            ClientCommand::PushChunk {
                peer: p,
                address: a,
                chunk,
                response,
            } => {
                assert_eq!(p, peer);
                assert_eq!(a, address);
                assert_eq!(*chunk.address(), address);
                response
            }
            other => panic!("unexpected command: {other:?}"),
        };

        // Resolve the request through its own response channel, as the
        // handler does when the verified receipt arrives on the request's
        // substream.
        let (verified, storer) = signed_receipt(&address);
        response.send(Ok(verified)).expect("receiver alive");

        let receipt = push.await.unwrap().expect("push resolves");
        assert_eq!(receipt.storer, storer);
        assert_eq!(receipt.address, address);
        assert_eq!(receipt.nonce, Nonce::from([9u8; 32]));
        assert_eq!(receipt.storage_radius.get(), 5);
    }

    #[tokio::test]
    async fn put_prefers_affordable_candidate_over_closer_unaffordable_one() {
        use crate::{PeerScores, PeerSelector, SettlementTrigger};
        use vertex_swarm_api::{Au, PeerAffordability, SwarmPricing};

        struct NoScores;
        impl PeerScores for NoScores {
            fn peer_score(&self, _overlay: &OverlayAddress) -> Option<f64> {
                None
            }
        }

        struct AllUnaffordableExcept(OverlayAddress);
        impl PeerAffordability for AllUnaffordableExcept {
            fn can_afford(&self, overlay: &OverlayAddress, _price: Au) -> bool {
                *overlay == self.0
            }

            fn allowance_remaining(&self, _overlay: &OverlayAddress) -> Au {
                Au::ZERO
            }
        }

        struct UnitPricer;
        impl SwarmPricing for UnitPricer {
            fn price(&self, _chunk: &ChunkAddress) -> Au {
                Au::from_amount(1)
            }

            fn peer_price(&self, _peer: &OverlayAddress, _chunk: &ChunkAddress) -> Au {
                Au::from_amount(1)
            }
        }

        struct NoSettlement;
        impl SettlementTrigger for NoSettlement {
            fn trigger_settlement(&self, _peer: OverlayAddress) {}
        }

        let closer = OverlayAddress::from([1u8; 32]);
        let affordable = OverlayAddress::from([2u8; 32]);
        let selector = Arc::new(PeerSelector::new(
            Arc::new(NoScores),
            Arc::new(AllUnaffordableExcept(affordable)),
            Arc::new(UnitPricer),
            Arc::new(NoSettlement),
        ));

        let (tx, mut rx) = mpsc::channel::<ClientCommand>(16);
        let handle = ClientHandle::new(tx);
        let topology = MockTopology::default().with_closest(vec![closer, affordable]);
        let bandwidth = Arc::new(Accounting::new(
            DefaultBandwidthConfig::default(),
            test_identity(),
        ));
        let pricer = FixedPricer::new(10_000, vertex_swarm_spec::init_mainnet());
        let accounting = ClientAccounting::new(bandwidth, pricer);
        let client = Client::client(topology, accounting, handle.clone()).with_selector(selector);

        let stamped = test_stamped_chunk();
        let put = tokio::spawn(async move { client.put(stamped).await });

        // The push must target the affordable candidate even though another
        // candidate is closer in proximity order.
        let cmd = rx.recv().await.expect("command emitted");
        match cmd {
            ClientCommand::PushChunk {
                peer,
                address,
                response,
                ..
            } => {
                assert_eq!(peer, affordable);
                let (signed, _) = signed_receipt(&address);
                response.send(Ok(signed)).expect("receiver alive");
            }
            other => panic!("unexpected command: {other:?}"),
        }

        put.await.unwrap().expect("push resolves");
    }

    #[tokio::test]
    async fn push_chunk_fails_when_push_is_rejected() {
        let (tx, mut rx) = mpsc::channel::<ClientCommand>(16);
        let handle = ClientHandle::new(tx);

        let peer = test_peer();
        let stamped = test_stamped_chunk();

        let push = {
            let handle = handle.clone();
            tokio::spawn(async move { handle.push_chunk(peer, stamped).await })
        };

        // The handler reports a storer rejection through the request's
        // response channel.
        match rx.recv().await.expect("command emitted") {
            ClientCommand::PushChunk { response, .. } => {
                response
                    .send(Err(ChunkTransferError::Remote))
                    .expect("receiver alive");
            }
            other => panic!("unexpected command: {other:?}"),
        }

        let result = push.await.unwrap();
        match result {
            Err(ChunkTransferError::Remote) => {}
            other => panic!("expected Remote, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn concurrent_retrievals_for_same_address_resolve_independently() {
        use crate::RetrievalResult;

        let (tx, mut rx) = mpsc::channel::<ClientCommand>(16);
        let handle = ClientHandle::new(tx);

        let address = test_address();
        let peer_a = OverlayAddress::from([1u8; 32]);
        let peer_b = OverlayAddress::from([2u8; 32]);

        let retrieval_a = {
            let handle = handle.clone();
            tokio::spawn(async move { handle.retrieve_chunk(peer_a, address).await })
        };
        let retrieval_b = {
            let handle = handle.clone();
            tokio::spawn(async move { handle.retrieve_chunk(peer_b, address).await })
        };

        // Both requests for the same address are in flight; collect their
        // response channels keyed by peer.
        let mut responses = std::collections::HashMap::new();
        for _ in 0..2 {
            match rx.recv().await.expect("command emitted") {
                ClientCommand::RetrieveChunk {
                    peer,
                    address: requested,
                    response,
                } => {
                    assert_eq!(requested, address);
                    responses.insert(peer, response);
                }
                other => panic!("unexpected command: {other:?}"),
            }
        }
        assert_eq!(responses.len(), 2, "requests must not alias");

        // Fail A and succeed B: each request resolves through its own channel.
        responses
            .remove(&peer_a)
            .unwrap()
            .send(Err(ChunkTransferError::Protocol("missing".to_string())))
            .expect("receiver alive");
        responses
            .remove(&peer_b)
            .unwrap()
            .send(Ok(RetrievalResult {
                chunk: test_chunk(),
                stamp: Some(test_stamp()),
                peer: peer_b,
            }))
            .expect("receiver alive");

        let err = retrieval_a.await.unwrap().unwrap_err();
        assert!(matches!(err, ChunkTransferError::Protocol(_)));
        let result = retrieval_b.await.unwrap().expect("b resolves");
        assert_eq!(result.peer, peer_b);
    }

    #[tokio::test]
    async fn get_starts_next_candidate_on_failure_and_returns_first_success() {
        use crate::RetrievalResult;

        let peer_a = OverlayAddress::from([1u8; 32]);
        let peer_b = OverlayAddress::from([2u8; 32]);

        let (tx, mut rx) = mpsc::channel::<ClientCommand>(16);
        let handle = ClientHandle::new(tx);
        let topology = MockTopology::default().with_closest(vec![peer_a, peer_b]);
        let bandwidth = Arc::new(Accounting::new(
            DefaultBandwidthConfig::default(),
            test_identity(),
        ));
        let pricer = FixedPricer::new(10_000, vertex_swarm_spec::init_mainnet());
        let accounting = ClientAccounting::new(bandwidth, pricer);
        let client = Client::client(topology, accounting, handle);

        let address = test_address();
        let get = tokio::spawn(async move { client.get(&address).await });

        // First candidate fails; the next must start immediately (well before
        // the stagger interval).
        match rx.recv().await.expect("first command") {
            ClientCommand::RetrieveChunk { peer, response, .. } => {
                assert_eq!(peer, peer_a);
                response
                    .send(Err(ChunkTransferError::Protocol("missing".to_string())))
                    .expect("receiver alive");
            }
            other => panic!("unexpected command: {other:?}"),
        }
        match rx.recv().await.expect("second command") {
            ClientCommand::RetrieveChunk { peer, response, .. } => {
                assert_eq!(peer, peer_b);
                response
                    .send(Ok(RetrievalResult {
                        chunk: test_chunk(),
                        stamp: Some(test_stamp()),
                        peer: peer_b,
                    }))
                    .expect("receiver alive");
            }
            other => panic!("unexpected command: {other:?}"),
        }

        let chunk = get.await.unwrap().expect("get resolves");
        assert_eq!(*chunk.address(), test_address());
    }

    #[tokio::test]
    async fn get_staggers_in_a_second_candidate_while_the_first_is_silent() {
        use crate::RetrievalResult;

        let peer_a = OverlayAddress::from([1u8; 32]);
        let peer_b = OverlayAddress::from([2u8; 32]);

        let (tx, mut rx) = mpsc::channel::<ClientCommand>(16);
        let handle = ClientHandle::new(tx);
        let topology = MockTopology::default().with_closest(vec![peer_a, peer_b]);
        let bandwidth = Arc::new(Accounting::new(
            DefaultBandwidthConfig::default(),
            test_identity(),
        ));
        let pricer = FixedPricer::new(10_000, vertex_swarm_spec::init_mainnet());
        let accounting = ClientAccounting::new(bandwidth, pricer);
        let client = Client::client(topology, accounting, handle);

        let address = test_address();
        let get = tokio::spawn(async move { client.get(&address).await });

        // Leave the first candidate unanswered; the stagger must bring in the
        // second, and its response must resolve the race.
        match rx.recv().await.expect("first command") {
            ClientCommand::RetrieveChunk { peer, .. } => assert_eq!(peer, peer_a),
            other => panic!("unexpected command: {other:?}"),
        }
        match rx.recv().await.expect("second command after stagger") {
            ClientCommand::RetrieveChunk { peer, response, .. } => {
                assert_eq!(peer, peer_b);
                response
                    .send(Ok(RetrievalResult {
                        chunk: test_chunk(),
                        stamp: Some(test_stamp()),
                        peer: peer_b,
                    }))
                    .expect("receiver alive");
            }
            other => panic!("unexpected command: {other:?}"),
        }

        let chunk = get.await.unwrap().expect("get resolves");
        assert_eq!(*chunk.address(), test_address());
    }
}
