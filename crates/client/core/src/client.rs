//! SwarmClient implementation.

use std::sync::Arc;

use async_trait::async_trait;
use vertex_bandwidth_core::Pricer;
use vertex_primitives::{AnyChunk, ChunkAddress};
use vertex_swarm_api::{AvailabilityAccounting, SwarmError, SwarmReader, SwarmResult, Topology};

use crate::service::ClientHandle;

/// Light client for Swarm chunk retrieval.
///
/// Provides read-only access to the Swarm network with bandwidth accounting.
pub struct SwarmClient<T, A, P>
where
    T: Topology,
    A: AvailabilityAccounting,
    P: Pricer,
{
    topology: Arc<T>,
    accounting: Arc<A>,
    pricer: Arc<P>,
    client_handle: ClientHandle,
}

impl<T, A, P> SwarmClient<T, A, P>
where
    T: Topology,
    A: AvailabilityAccounting,
    P: Pricer,
{
    /// Create a new SwarmClient.
    pub fn new(topology: T, accounting: A, pricer: P, client_handle: ClientHandle) -> Self {
        Self {
            topology: Arc::new(topology),
            accounting: Arc::new(accounting),
            pricer: Arc::new(pricer),
            client_handle,
        }
    }

    /// Create from Arc-wrapped components.
    pub fn from_arcs(
        topology: Arc<T>,
        accounting: Arc<A>,
        pricer: Arc<P>,
        client_handle: ClientHandle,
    ) -> Self {
        Self {
            topology,
            accounting,
            pricer,
            client_handle,
        }
    }

    /// Get the pricer.
    pub fn pricer(&self) -> &P {
        &self.pricer
    }

    /// Get the client handle.
    pub fn client_handle(&self) -> &ClientHandle {
        &self.client_handle
    }
}

impl<T, A, P> Clone for SwarmClient<T, A, P>
where
    T: Topology,
    A: AvailabilityAccounting,
    P: Pricer,
{
    fn clone(&self) -> Self {
        Self {
            topology: Arc::clone(&self.topology),
            accounting: Arc::clone(&self.accounting),
            pricer: Arc::clone(&self.pricer),
            client_handle: self.client_handle.clone(),
        }
    }
}

#[async_trait]
impl<T, A, P> SwarmReader for SwarmClient<T, A, P>
where
    T: Topology + 'static,
    A: AvailabilityAccounting + 'static,
    P: Pricer + 'static,
{
    type Topology = T;
    type Accounting = A;

    fn topology(&self) -> &Self::Topology {
        &self.topology
    }

    fn accounting(&self) -> &Self::Accounting {
        &self.accounting
    }

    async fn get(&self, address: &ChunkAddress) -> SwarmResult<AnyChunk> {
        // Get closest peers to the chunk address
        let _closest = self.topology.closest_to(address, 3);

        // TODO: Try each peer in order of proximity
        // For each peer:
        // 1. Check availability allowance
        // 2. Retrieve the chunk
        // 3. Record bandwidth usage

        let _handle = &self.client_handle;
        let _pricer = &self.pricer;
        let _accounting = &self.accounting;

        Err(SwarmError::ChunkNotFound { address: *address })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;
    use vertex_bandwidth_core::{Accounting, AccountingConfig, FixedPricer};
    use vertex_net_client::ClientCommand;
    use vertex_primitives::OverlayAddress;

    /// Mock topology for testing.
    #[derive(Clone, Default)]
    struct MockTopology {
        self_addr: OverlayAddress,
    }

    impl Topology for MockTopology {
        fn self_address(&self) -> OverlayAddress {
            self.self_addr
        }

        fn neighbors(&self, _depth: u8) -> Vec<OverlayAddress> {
            Vec::new()
        }

        fn is_responsible_for(&self, _address: &ChunkAddress) -> bool {
            false
        }

        fn depth(&self) -> u8 {
            0
        }

        fn closest_to(&self, _address: &ChunkAddress, _count: usize) -> Vec<OverlayAddress> {
            Vec::new()
        }

        fn add_peers(&self, _peers: &[OverlayAddress]) {}

        fn pick(&self, _peer: &OverlayAddress, _is_full_node: bool) -> bool {
            true
        }

        fn connected(&self, _peer: OverlayAddress) {}

        fn disconnected(&self, _peer: &OverlayAddress) {}

        fn peers_to_connect(&self) -> Vec<OverlayAddress> {
            Vec::new()
        }
    }

    fn create_test_handle() -> ClientHandle {
        let (tx, _rx) = mpsc::unbounded_channel::<ClientCommand>();
        ClientHandle::new(tx)
    }

    #[test]
    fn test_client_clone() {
        let topology = MockTopology::default();
        let accounting = Accounting::new(AccountingConfig::default());
        let pricer = FixedPricer::default();
        let handle = create_test_handle();

        let client = SwarmClient::new(topology, accounting, pricer, handle);
        let _clone = client.clone();
    }

    #[test]
    fn test_client_accounting() {
        let topology = MockTopology::default();
        let accounting = Accounting::new(AccountingConfig::default());
        let pricer = FixedPricer::default();
        let handle = create_test_handle();

        let client = SwarmClient::new(topology, accounting, pricer, handle);

        let peers = client.accounting().peers();
        assert!(peers.is_empty());
    }
}
