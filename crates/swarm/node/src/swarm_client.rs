//! Unified client for Swarm nodes.

use async_trait::async_trait;
use nectar_primitives::{AnyChunk, ChunkAddress, ContentChunk};
use vertex_swarm_api::{
    BootnodeComponents, ClientComponents, HasAccounting, HasTopology, SwarmClient,
    SwarmClientAccounting, SwarmError, SwarmResult, SwarmTopologyRouting,
};

use crate::ClientHandle;

/// Number of closest peers to attempt for retrieval and push operations.
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

/// Storage proof for an upload: the pre-computed postage stamp bytes that
/// accompany a chunk on the pushsync wire.
///
/// `put` is the client-side entry point for already-stamped chunks. The stamp
/// is supplied as the storage proof rather than derived here, so the upload
/// layer owns batch selection and stamp signing.
#[async_trait]
impl<T, A, S> SwarmClient for Client<ClientComponents<T, A>, S>
where
    T: SwarmTopologyRouting + Send + Sync + 'static,
    A: SwarmClientAccounting + Send + Sync + 'static,
    S: AsRef<[u8]> + Send + Sync + 'static,
{
    type Storage = S;

    async fn get(&self, address: &ChunkAddress) -> SwarmResult<AnyChunk> {
        let closest = self
            .components
            .topology()
            .closest_to(address, CLOSEST_PEER_COUNT);

        if closest.is_empty() {
            return Err(SwarmError::NoStorer {
                chunk_address: *address,
            });
        }

        let mut last_error: Option<SwarmError> = None;
        for peer in closest {
            match self.client_handle.retrieve_chunk(peer, *address).await {
                Ok(result) => {
                    let chunk = ContentChunk::try_from(result.data).map_err(|e| {
                        SwarmError::InvalidChunk {
                            address: Some(*address),
                            reason: e.to_string(),
                        }
                    })?;
                    let chunk: AnyChunk = chunk.into();
                    chunk
                        .verify(address)
                        .map_err(|e| SwarmError::InvalidChunk {
                            address: Some(*address),
                            reason: e.to_string(),
                        })?;
                    return Ok(chunk);
                }
                Err(e) => {
                    last_error = Some(SwarmError::network_msg(e.to_string()));
                }
            }
        }

        Err(last_error.unwrap_or(SwarmError::ChunkNotFound { address: *address }))
    }

    async fn put(&self, chunk: AnyChunk, storage: &Self::Storage) -> SwarmResult<()> {
        let address = *chunk.address();
        let closest = self
            .components
            .topology()
            .closest_to(&address, CLOSEST_PEER_COUNT);

        let peer = closest.into_iter().next().ok_or(SwarmError::NoStorer {
            chunk_address: address,
        })?;

        let stamp = bytes::Bytes::copy_from_slice(storage.as_ref());
        let data = chunk.into_bytes();

        self.client_handle
            .push_chunk(peer, address, data, stamp)
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
    use crate::client_service::PushResult;
    use vertex_swarm_primitives::OverlayAddress;

    fn test_peer() -> OverlayAddress {
        OverlayAddress::from([7u8; 32])
    }

    fn test_address() -> ChunkAddress {
        ChunkAddress::new([0x11; 32])
    }

    #[tokio::test]
    async fn push_chunk_emits_command_and_resolves_on_receipt() {
        let (tx, mut rx) = mpsc::channel::<ClientCommand>(16);
        let handle = ClientHandle::new(tx);

        let peer = test_peer();
        let address = test_address();
        let data = bytes::Bytes::from_static(b"chunk-bytes");
        let stamp = bytes::Bytes::from_static(b"stamp-bytes");

        let push = {
            let handle = handle.clone();
            tokio::spawn(async move { handle.push_chunk(peer, address, data, stamp).await })
        };

        // The handle must have emitted a PushChunk command for the closest peer.
        let cmd = rx.recv().await.expect("command emitted");
        match cmd {
            ClientCommand::PushChunk {
                peer: p,
                address: a,
                data,
                stamp,
            } => {
                assert_eq!(p, peer);
                assert_eq!(a, address);
                assert_eq!(data.as_ref(), b"chunk-bytes");
                assert_eq!(stamp.as_ref(), b"stamp-bytes");
            }
            other => panic!("unexpected command: {other:?}"),
        }

        // Simulate the event processor delivering a receipt.
        handle.complete_push(
            address,
            PushResult {
                peer,
                signature: bytes::Bytes::from_static(b"sig"),
                nonce: bytes::Bytes::from_static(b"nonce"),
                storage_radius: 5,
            },
        );

        let receipt = push.await.unwrap().expect("push resolves");
        assert_eq!(receipt.peer, peer);
        assert_eq!(receipt.signature.as_ref(), b"sig");
        assert_eq!(receipt.storage_radius, 5);
    }

    #[tokio::test]
    async fn push_chunk_fails_when_push_is_rejected() {
        let (tx, mut rx) = mpsc::channel::<ClientCommand>(16);
        let handle = ClientHandle::new(tx);

        let peer = test_peer();
        let address = test_address();

        let push = {
            let handle = handle.clone();
            tokio::spawn(async move {
                handle
                    .push_chunk(
                        peer,
                        address,
                        bytes::Bytes::from_static(b"d"),
                        bytes::Bytes::from_static(b"s"),
                    )
                    .await
            })
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
