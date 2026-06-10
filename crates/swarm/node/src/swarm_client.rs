//! Unified client for Swarm nodes.

use async_trait::async_trait;
use nectar_primitives::{AnyChunk, ChunkAddress};
use vertex_swarm_api::{
    BootnodeComponents, ClientComponents, HasAccounting, HasTopology, StampedChunk, SwarmClient,
    SwarmClientAccounting, SwarmError, SwarmResult, SwarmTopologyRouting,
};

use crate::ClientHandle;

/// Number of closest peers to try, in order, for a retrieval.
const CLOSEST_PEER_COUNT: usize = 3;

/// Unified client for all Swarm node types.
///
/// Generic over component type `C`:
/// - [`BootnodeComponents<T>`] for bootnodes (topology only)
/// - [`ClientComponents<T, A>`] for client/storer nodes (topology + accounting)
pub struct Client<C, S = ()> {
    components: C,
    client_handle: ClientHandle,
    _storage: std::marker::PhantomData<S>,
}

impl<C, S> Client<C, S> {
    /// Create a client from components.
    pub fn new(components: C, client_handle: ClientHandle) -> Self {
        Self {
            components,
            client_handle,
            _storage: std::marker::PhantomData,
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
        let attempts = closest.len();

        // Walk the closest peers in order and return the first response. The
        // codec reconstructs the chunk against the requested address, so the
        // retrieved chunk is already address-validated; no re-verification is
        // needed here. Retrieval cannot yet be raced across peers for the same
        // chunk: the client handle correlates a response to a pending request by
        // chunk address alone, so two in-flight requests for one address would
        // alias. Fanning out needs a per-request correlation id in the handle
        // and handler, tracked as a follow-up; until then this is sequential.
        //
        // The seed error covers the no-candidates case; each failed attempt
        // replaces it, so the value after the loop is always the last failure.
        let mut outcome = Err(SwarmError::NoStorer {
            chunk_address: *address,
        });
        for peer in closest {
            match self.client_handle.retrieve_chunk(peer, *address).await {
                Ok(result) => return Ok(result.chunk.into_parts().0),
                Err(e) => {
                    outcome = Err(SwarmError::AllPeersFailed {
                        address: *address,
                        attempts,
                        source: Box::new(e),
                    });
                }
            }
        }

        outcome
    }

    async fn put(&self, chunk: StampedChunk) -> SwarmResult<()> {
        let address = *chunk.address();
        let closest = self
            .components
            .topology()
            .closest_to(&address, CLOSEST_PEER_COUNT);

        let peer = closest.into_iter().next().ok_or(SwarmError::NoStorer {
            chunk_address: address,
        })?;

        self.client_handle
            .push_chunk(peer, chunk)
            .await
            .map(|_receipt| ())
            .map_err(|e| SwarmError::network_msg(e.to_string()))
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

    use crate::RetrievalError;
    use alloy_primitives::{B256, Signature};
    use nectar_primitives::{ContentChunk, Nonce};
    use vertex_swarm_api::{PushReceipt, Stamp};
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

    fn test_stamp() -> Stamp {
        Stamp::new(B256::repeat_byte(0xaa), 3, 7, 42, test_signature())
    }

    fn test_stamped_chunk() -> StampedChunk {
        let chunk = ContentChunk::new(&b"chunk-bytes"[..]).expect("valid content chunk");
        StampedChunk::new(chunk.into(), test_stamp())
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
        match cmd {
            ClientCommand::PushChunk {
                peer: p,
                address: a,
                chunk,
            } => {
                assert_eq!(p, peer);
                assert_eq!(a, address);
                assert_eq!(*chunk.address(), address);
            }
            other => panic!("unexpected command: {other:?}"),
        }

        // Simulate the event processor delivering a typed receipt.
        handle.complete_push(
            address,
            PushReceipt {
                storer: peer,
                signature: test_signature(),
                nonce: Nonce::from([9u8; 32]),
                storage_radius: StorageRadius::new(Bin::new(5).unwrap()),
            },
        );

        let receipt = push.await.unwrap().expect("push resolves");
        assert_eq!(receipt.storer, peer);
        assert_eq!(receipt.signature, test_signature());
        assert_eq!(receipt.nonce, Nonce::from([9u8; 32]));
        assert_eq!(receipt.storage_radius.get(), 5);
    }

    #[tokio::test]
    async fn push_chunk_fails_when_push_is_rejected() {
        let (tx, mut rx) = mpsc::channel::<ClientCommand>(16);
        let handle = ClientHandle::new(tx);

        let peer = test_peer();
        let stamped = test_stamped_chunk();
        let address = test_address();

        let push = {
            let handle = handle.clone();
            tokio::spawn(async move { handle.push_chunk(peer, stamped).await })
        };

        let _ = rx.recv().await.expect("command emitted");

        // The event processor reports a storer rejection.
        handle.fail_push(address, "rejected".to_string());

        let result = push.await.unwrap();
        match result {
            Err(RetrievalError::PushRejected(reason)) => assert_eq!(reason, "rejected"),
            other => panic!("expected PushRejected, got {other:?}"),
        }
    }
}
