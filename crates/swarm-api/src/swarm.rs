//! Core Swarm traits for network access.
//!
//! This module defines the fundamental interfaces for interacting with
//! the Swarm network:
//!
//! - [`SwarmReader`] - Read-only access (get chunks)
//! - [`SwarmWriter`] - Read-write access (put and get chunks)
//!
//! # Availability Accounting
//!
//! Even read-only operations require availability accounting for
//! retrieval incentives (pseudosettle, SWAP). The `Accounting`
//! associated type provides per-peer availability tracking.
//!
//! # Example
//!
//! ```ignore
//! // Read-only light client
//! impl SwarmReader for LightClient {
//!     type Accounting = NoAvailabilityIncentives;
//!
//!     fn accounting(&self) -> &Self::Accounting {
//!         &self.accounting
//!     }
//!
//!     async fn get(&self, address: &ChunkAddress) -> Result<AnyChunk> {
//!         // Retrieve from network
//!     }
//! }
//!
//! // Full node with storage
//! impl SwarmWriter for FullNode {
//!     type Storage = Stamp;  // Postage stamps
//!
//!     async fn put(&self, chunk: AnyChunk, storage: &Self::Storage) -> Result<()> {
//!         // Store with storage proof
//!     }
//! }
//! ```

use crate::{AvailabilityAccounting, SwarmResult, Topology};
use async_trait::async_trait;
use vertex_primitives::{AnyChunk, ChunkAddress};

// ============================================================================
// SwarmReader - Read-only access
// ============================================================================

/// Read-only access to the Swarm network.
///
/// Use this for light clients that only retrieve data. Even read operations
/// require topology awareness and availability accounting.
#[async_trait]
pub trait SwarmReader: Send + Sync {
    /// The topology implementation for peer discovery and routing.
    type Topology: Topology;

    /// The availability accounting factory for retrieval incentives.
    type Accounting: AvailabilityAccounting;

    /// Get the topology for finding peers.
    fn topology(&self) -> &Self::Topology;

    /// Get the availability accounting factory.
    fn accounting(&self) -> &Self::Accounting;

    /// Get a chunk from the swarm by its address.
    ///
    /// The implementation should:
    /// 1. Find peers closest to the chunk: `topology.closest_to(address, count)`
    /// 2. Check availability allowance: `accounting.for_peer(peer).allow(size)`
    /// 3. Retrieve the chunk
    /// 4. Record usage: `accounting.for_peer(peer).record(size, Direction::Download)`
    async fn get(&self, address: &ChunkAddress) -> SwarmResult<AnyChunk>;
}

// ============================================================================
// SwarmWriter - Read-write access
// ============================================================================

/// Read-write access to the Swarm network.
///
/// Extends [`SwarmReader`] with the ability to store chunks. The `Storage`
/// type determines what proof of storage incentive is required.
///
/// # Storage
///
/// - Use postage `Stamp` for mainnet (proof from valid batch)
/// - Use `()` for dev/testing networks without storage incentives
///
/// # Example
///
/// ```ignore
/// impl SwarmWriter for MainnetNode {
///     type Storage = nectar_postage::Stamp;
///
///     async fn put(&self, chunk: AnyChunk, storage: &Self::Storage) -> Result<()> {
///         // Validate stamp
///         // Store chunk
///         // Push to neighbors
///     }
/// }
/// ```
#[async_trait]
pub trait SwarmWriter: SwarmReader {
    /// The storage incentive proof type required for storing chunks.
    ///
    /// This is passed to `put()` to authorize storage. The type should match
    /// `PublisherNodeTypes::Storage` when used with node components.
    type Storage: Send + Sync;

    /// Put a chunk into the swarm with storage proof.
    ///
    /// The implementation should:
    /// 1. Validate the storage proof (postage stamp)
    /// 2. Store locally if responsible
    /// 3. Forward to neighbors (push sync)
    /// 4. Record bandwidth for uploads
    async fn put(&self, chunk: AnyChunk, storage: &Self::Storage) -> SwarmResult<()>;
}
