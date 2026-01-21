//! Vertex node types and components
//!
//! This module defines the concrete types that make up the Vertex Swarm node,
//! implementing the node-types traits with actual implementations.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, RwLock};
use vertex_node_api::{
    async_trait, FullNodeTypes, LocalStore, NodeTypes, PublisherNodeTypes, SyncResult, Topology,
};
use vertex_primitives::{AnyChunk, ChunkAddress, OverlayAddress, StandardChunkSet};
use vertex_swarm_api::{
    BandwidthAccounting, ChunkSync, Direction, NoBandwidthIncentives, PeerBandwidth, SwarmError,
    SwarmReader, SwarmResult, SwarmWriter,
};
use vertex_swarmspec::Hive;

// ============================================================================
// Vertex Node Types
// ============================================================================

/// The main Vertex node type configuration.
///
/// This type carries all the associated types needed for a Vertex full node.
#[derive(Clone, Debug)]
pub struct VertexNodeTypes;

impl NodeTypes for VertexNodeTypes {
    type Spec = Hive;
    type ChunkSet = StandardChunkSet;
    type Topology = VertexTopology;
    type DataAvailability = VertexBandwidth;
}

impl PublisherNodeTypes for VertexNodeTypes {
    /// No payment required in dev mode
    type StoragePayment = ();
}

impl FullNodeTypes for VertexNodeTypes {
    type Store = VertexStore;
    type Sync = VertexSync;
}

// ============================================================================
// Dev Node Types (no incentives)
// ============================================================================

/// Development node type with no incentives.
///
/// Useful for local testing without bandwidth or storage incentives.
#[derive(Clone, Debug)]
pub struct DevNodeTypes;

impl NodeTypes for DevNodeTypes {
    type Spec = Hive;
    type ChunkSet = StandardChunkSet;
    type Topology = VertexTopology;
    type DataAvailability = NoBandwidthIncentives;
}

impl PublisherNodeTypes for DevNodeTypes {
    type StoragePayment = ();
}

impl FullNodeTypes for DevNodeTypes {
    type Store = VertexStore;
    type Sync = VertexSync;
}

// ============================================================================
// Topology Implementation
// ============================================================================

/// Vertex topology implementation
#[derive(Debug, Clone)]
pub struct VertexTopology {
    /// Our own overlay address
    self_addr: OverlayAddress,
    /// Depth of our neighborhood
    depth: Arc<RwLock<u8>>,
    /// Known peers (by overlay address)
    peers: Arc<RwLock<Vec<OverlayAddress>>>,
}

impl VertexTopology {
    /// Create a new topology with the given self address.
    pub fn new(self_addr: OverlayAddress) -> Self {
        Self {
            self_addr,
            depth: Arc::new(RwLock::new(0)),
            peers: Arc::new(RwLock::new(Vec::new())),
        }
    }
}

impl Default for VertexTopology {
    fn default() -> Self {
        Self::new(OverlayAddress::default())
    }
}

impl Topology for VertexTopology {
    fn self_address(&self) -> OverlayAddress {
        self.self_addr
    }

    fn neighbors(&self, _depth: u8) -> Vec<OverlayAddress> {
        self.peers.read().unwrap().clone()
    }

    fn is_responsible_for(&self, _address: &ChunkAddress) -> bool {
        // TODO: Implement proper responsibility check based on Kademlia
        false
    }

    fn depth(&self) -> u8 {
        *self.depth.read().unwrap()
    }

    fn closest_to(&self, _address: &ChunkAddress, count: usize) -> Vec<OverlayAddress> {
        // TODO: Implement proper closest peer lookup using XOR distance
        let peers = self.peers.read().unwrap();
        peers.iter().take(count).cloned().collect()
    }
}

// ============================================================================
// Store Implementation
// ============================================================================

/// Vertex local store implementation
#[derive(Debug, Clone, Default)]
pub struct VertexStore {
    /// In-memory store (placeholder for real implementation)
    chunks: Arc<RwLock<HashMap<ChunkAddress, AnyChunk>>>,
}

impl VertexStore {
    /// Create a new store
    pub fn new() -> Self {
        Self::default()
    }
}

impl LocalStore for VertexStore {
    fn store(&self, chunk: &AnyChunk) -> SwarmResult<()> {
        let address = *chunk.address();
        self.chunks.write().unwrap().insert(address, chunk.clone());
        Ok(())
    }

    fn retrieve(&self, address: &ChunkAddress) -> SwarmResult<Option<AnyChunk>> {
        Ok(self.chunks.read().unwrap().get(address).cloned())
    }

    fn has(&self, address: &ChunkAddress) -> bool {
        self.chunks.read().unwrap().contains_key(address)
    }

    fn remove(&self, address: &ChunkAddress) -> SwarmResult<()> {
        self.chunks.write().unwrap().remove(address);
        Ok(())
    }
}

// ============================================================================
// Sync Implementation
// ============================================================================

/// Vertex chunk sync implementation
#[derive(Debug, Clone, Default)]
pub struct VertexSync;

impl VertexSync {
    /// Create a new sync handler
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ChunkSync for VertexSync {
    async fn sync_with(&self, _peer: &OverlayAddress) -> SwarmResult<SyncResult> {
        // TODO: Implement actual sync protocol
        Ok(SyncResult::default())
    }

    async fn offer(&self, _chunk: &AnyChunk) -> SwarmResult<()> {
        // TODO: Implement push sync
        Ok(())
    }
}

// ============================================================================
// Bandwidth Implementation (Lock-free per-peer accounting)
// ============================================================================

/// Per-peer bandwidth accounting state (internal, shared via Arc).
#[derive(Debug)]
struct PeerAccountingState {
    /// Peer's overlay address
    peer: OverlayAddress,
    /// Balance in bytes (positive = peer owes us).
    /// Uses atomic for lock-free record() operations.
    balance: AtomicI64,
    /// Disconnect threshold (bytes). If balance exceeds this, deny transfers.
    disconnect_threshold: i64,
}

/// Per-peer bandwidth accounting handle.
///
/// This is cloned and shared by all protocols on a connection.
/// The `record()` method is lock-free using atomic operations.
#[derive(Debug, Clone)]
pub struct VertexPeerBandwidth {
    state: Arc<PeerAccountingState>,
}

impl VertexPeerBandwidth {
    fn new(peer: OverlayAddress, disconnect_threshold: i64) -> Self {
        Self {
            state: Arc::new(PeerAccountingState {
                peer,
                balance: AtomicI64::new(0),
                disconnect_threshold,
            }),
        }
    }
}

#[async_trait]
impl PeerBandwidth for VertexPeerBandwidth {
    /// Record bandwidth usage (lock-free atomic operation).
    fn record(&self, bytes: u64, direction: Direction) {
        let delta = bytes as i64;
        match direction {
            // Download = peer sent us data, they used bandwidth, balance increases
            Direction::Download => {
                self.state.balance.fetch_add(delta, Ordering::Relaxed);
            }
            // Upload = we sent data to peer, we used bandwidth, balance decreases
            Direction::Upload => {
                self.state.balance.fetch_sub(delta, Ordering::Relaxed);
            }
        }
    }

    /// Check if a transfer is allowed (based on disconnect threshold).
    fn allow(&self, bytes: u64) -> bool {
        let current = self.state.balance.load(Ordering::Relaxed);
        // Deny if balance would exceed disconnect threshold
        current + (bytes as i64) <= self.state.disconnect_threshold
    }

    /// Get the current balance (positive = peer owes us).
    fn balance(&self) -> i64 {
        self.state.balance.load(Ordering::Relaxed)
    }

    async fn settle(&self) -> SwarmResult<()> {
        // TODO: Implement SWAP cheque settlement
        // For now, just reset the balance
        self.state.balance.store(0, Ordering::Relaxed);
        Ok(())
    }

    fn peer(&self) -> OverlayAddress {
        self.state.peer
    }
}

/// Vertex bandwidth accounting factory.
///
/// Creates per-peer accounting handles with lock-free operations.
/// The factory uses a RwLock for peer management (rare operations),
/// but individual peer accounting is lock-free.
#[derive(Debug, Clone, Default)]
pub struct VertexBandwidth {
    /// Map from overlay address to per-peer accounting.
    /// Uses RwLock for peer add/remove (infrequent), but each peer's
    /// accounting uses atomics for lock-free record().
    peers: Arc<RwLock<HashMap<OverlayAddress, VertexPeerBandwidth>>>,
    /// Disconnect threshold for new peers (bytes).
    disconnect_threshold: i64,
}

impl VertexBandwidth {
    /// Create a new bandwidth accounting factory.
    pub fn new() -> Self {
        Self::with_threshold(10 * 1024 * 1024 * 1024) // 10 GB default
    }

    /// Create with a custom disconnect threshold (in bytes).
    pub fn with_threshold(disconnect_threshold: i64) -> Self {
        Self {
            peers: Arc::default(),
            disconnect_threshold,
        }
    }
}

impl BandwidthAccounting for VertexBandwidth {
    type Peer = VertexPeerBandwidth;

    /// Get or create a per-peer accounting handle.
    ///
    /// This is called when a connection is established. The returned
    /// handle is cloned and shared by all protocols on that connection.
    fn for_peer(&self, peer: OverlayAddress) -> Self::Peer {
        // Fast path: check if peer already exists (read lock)
        {
            let peers = self.peers.read().unwrap();
            if let Some(handle) = peers.get(&peer) {
                return handle.clone();
            }
        }

        // Slow path: create new peer accounting (write lock)
        let mut peers = self.peers.write().unwrap();
        // Double-check after acquiring write lock
        if let Some(handle) = peers.get(&peer) {
            return handle.clone();
        }

        let handle = VertexPeerBandwidth::new(peer, self.disconnect_threshold);
        peers.insert(peer, handle.clone());
        handle
    }

    fn peers(&self) -> Vec<OverlayAddress> {
        self.peers.read().unwrap().keys().copied().collect()
    }

    fn remove_peer(&self, peer: &OverlayAddress) {
        self.peers.write().unwrap().remove(peer);
    }
}

// ============================================================================
// Swarm Implementation (Generic over Accounting)
// ============================================================================

/// Vertex Swarm client implementation.
///
/// Generic over the bandwidth accounting type, allowing different
/// accounting strategies (full accounting, no accounting, etc.)
#[derive(Debug, Clone)]
pub struct VertexSwarm<A: BandwidthAccounting> {
    /// Bandwidth accounting factory
    accounting: A,
    /// Local storage
    store: VertexStore,
    /// Topology
    topology: VertexTopology,
    /// Sync handler
    sync: VertexSync,
}

impl<A: BandwidthAccounting> VertexSwarm<A> {
    /// Create a new Swarm client
    pub fn new(
        accounting: A,
        store: VertexStore,
        topology: VertexTopology,
        sync: VertexSync,
    ) -> Self {
        Self {
            accounting,
            store,
            topology,
            sync,
        }
    }
}

#[async_trait]
impl<A: BandwidthAccounting + Send + Sync> SwarmReader for VertexSwarm<A> {
    type Accounting = A;

    fn accounting(&self) -> &Self::Accounting {
        &self.accounting
    }

    async fn get(&self, address: &ChunkAddress) -> SwarmResult<AnyChunk> {
        // Check local storage first
        if let Some(chunk) = self.store.retrieve(address)? {
            return Ok(chunk);
        }
        // TODO: Retrieve from network with bandwidth accounting
        // 1. Find peer with chunk via topology
        // 2. let peer_acct = self.accounting.for_peer(peer_id);
        // 3. if !peer_acct.allow(CHUNK_SIZE) { return Err(...) }
        // 4. Retrieve chunk
        // 5. peer_acct.record(bytes, Direction::Download);
        Err(SwarmError::ChunkNotFound { address: *address })
    }
}

#[async_trait]
impl<A: BandwidthAccounting + Send + Sync> SwarmWriter for VertexSwarm<A> {
    type Payment = ();

    async fn put(&self, chunk: AnyChunk, _payment: &Self::Payment) -> SwarmResult<()> {
        // Store locally if we're responsible
        if self.topology.is_responsible_for(chunk.address()) {
            self.store.store(&chunk)?;
        }
        // Push to neighbors (with bandwidth accounting)
        // TODO: For each neighbor, record upload bandwidth
        self.sync.offer(&chunk).await
    }
}

// ============================================================================
// Type Aliases
// ============================================================================

/// Development swarm with no bandwidth incentives.
pub type DevSwarm = VertexSwarm<NoBandwidthIncentives>;

/// Production swarm with full bandwidth accounting.
pub type ProdSwarm = VertexSwarm<VertexBandwidth>;

// ============================================================================
// Node Builder
// ============================================================================

/// Builder for creating Vertex node components
#[derive(Debug, Default)]
pub struct VertexNodeBuilder {
    store: Option<VertexStore>,
    topology: Option<VertexTopology>,
    sync: Option<VertexSync>,
    bandwidth: Option<VertexBandwidth>,
}

impl VertexNodeBuilder {
    /// Create a new node builder
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the store implementation
    pub fn with_store(mut self, store: VertexStore) -> Self {
        self.store = Some(store);
        self
    }

    /// Set the topology implementation
    pub fn with_topology(mut self, topology: VertexTopology) -> Self {
        self.topology = Some(topology);
        self
    }

    /// Set the sync implementation
    pub fn with_sync(mut self, sync: VertexSync) -> Self {
        self.sync = Some(sync);
        self
    }

    /// Set the bandwidth implementation
    pub fn with_bandwidth(mut self, bandwidth: VertexBandwidth) -> Self {
        self.bandwidth = Some(bandwidth);
        self
    }

    /// Build the Swarm client with full bandwidth accounting
    pub fn build_swarm(self) -> ProdSwarm {
        VertexSwarm::new(
            self.bandwidth.unwrap_or_default(),
            self.store.unwrap_or_default(),
            self.topology.unwrap_or_default(),
            self.sync.unwrap_or_default(),
        )
    }

    /// Build a dev swarm without bandwidth accounting
    pub fn build_dev_swarm(self) -> DevSwarm {
        VertexSwarm::new(
            NoBandwidthIncentives,
            self.store.unwrap_or_default(),
            self.topology.unwrap_or_default(),
            self.sync.unwrap_or_default(),
        )
    }

    /// Build all components
    pub fn build(self) -> BuiltComponents {
        let store = self.store.unwrap_or_default();
        let topology = self.topology.unwrap_or_default();
        let sync = self.sync.unwrap_or_default();
        let bandwidth = self.bandwidth.unwrap_or_default();
        let swarm = VertexSwarm::new(
            bandwidth.clone(),
            store.clone(),
            topology.clone(),
            sync.clone(),
        );

        BuiltComponents {
            swarm,
            store,
            topology,
            sync,
            bandwidth,
        }
    }
}

/// All built node components
#[derive(Debug, Clone)]
pub struct BuiltComponents {
    /// The swarm client
    pub swarm: ProdSwarm,
    /// Local storage
    pub store: VertexStore,
    /// Topology
    pub topology: VertexTopology,
    /// Sync handler
    pub sync: VertexSync,
    /// Bandwidth incentives (data availability)
    pub bandwidth: VertexBandwidth,
}
