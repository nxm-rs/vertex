//! Core Swarm traits for network access.
//!
//! This module defines the fundamental interfaces for interacting with
//! the Swarm network:
//!
//! - [`SwarmReader`] - Read-only access (get chunks)
//! - [`SwarmWriter`] - Read-write access (put and get chunks)
//!
//! # Bandwidth Accounting
//!
//! Even read-only operations require bandwidth accounting for
//! retrieval incentives (pseudosettle, SWAP). The `Accounting`
//! associated type provides per-peer bandwidth tracking.
//!
//! # Example
//!
//! ```ignore
//! // Read-only light client
//! impl SwarmReader for LightClient {
//!     type Accounting = NoBandwidthIncentives;
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
//!     type Payment = Stamp;  // Postage stamps
//!
//!     async fn put(&self, chunk: AnyChunk, payment: &Self::Payment) -> Result<()> {
//!         // Store with payment proof
//!     }
//! }
//! ```

use crate::BandwidthAccounting;
use async_trait::async_trait;
use vertex_primitives::{AnyChunk, ChunkAddress, Result};

// ============================================================================
// SwarmReader - Read-only access
// ============================================================================

/// Read-only access to the Swarm network.
///
/// Use this for light clients that only retrieve data. Even read operations
/// require bandwidth accounting to enable retrieval incentives (pseudosettle
/// and/or SWAP payment channels).
///
/// # Accounting
///
/// The `Accounting` associated type provides per-peer bandwidth tracking.
/// Implementations should use `self.accounting().for_peer(peer_id)` to get
/// a per-peer handle, then call `record()` after each chunk transfer.
#[async_trait]
pub trait SwarmReader: Send + Sync {
    /// The bandwidth accounting factory for retrieval incentives.
    ///
    /// Even read-only nodes must account for bandwidth consumed when
    /// retrieving chunks. This enables pseudosettle and/or SWAP.
    type Accounting: BandwidthAccounting;

    /// Get the bandwidth accounting factory.
    ///
    /// Use this to access per-peer accounting handles for bandwidth tracking.
    fn accounting(&self) -> &Self::Accounting;

    /// Get a chunk from the swarm by its address.
    ///
    /// The implementation should:
    /// 1. Find a peer that has the chunk
    /// 2. Check bandwidth allowance: `accounting.for_peer(peer).allow(size)`
    /// 3. Retrieve the chunk
    /// 4. Record bandwidth: `accounting.for_peer(peer).record(size, Direction::Download)`
    async fn get(&self, address: &ChunkAddress) -> Result<AnyChunk>;
}

// ============================================================================
// SwarmWriter - Read-write access
// ============================================================================

/// Read-write access to the Swarm network.
///
/// Extends [`SwarmReader`] with the ability to store chunks. The `Payment`
/// type determines what proof of payment is required for storage.
///
/// # Payment
///
/// - Use postage `Stamp` for mainnet (proof of payment from valid batch)
/// - Use `()` for dev/testing networks without payment requirement
///
/// # Example
///
/// ```ignore
/// impl SwarmWriter for MainnetNode {
///     type Payment = nectar_postage::Stamp;
///
///     async fn put(&self, chunk: AnyChunk, payment: &Self::Payment) -> Result<()> {
///         // Validate stamp
///         // Store chunk
///         // Push to neighbors
///     }
/// }
/// ```
#[async_trait]
pub trait SwarmWriter: SwarmReader {
    /// The payment/proof type required for storing chunks.
    ///
    /// This is passed to `put()` to authorize storage. The type should match
    /// `PublisherNodeTypes::StoragePayment` when used with node components.
    type Payment: Send + Sync;

    /// Put a chunk into the swarm with payment proof.
    ///
    /// The implementation should:
    /// 1. Validate the payment proof
    /// 2. Store locally if responsible
    /// 3. Forward to neighbors (push sync)
    /// 4. Record bandwidth for uploads
    async fn put(&self, chunk: AnyChunk, payment: &Self::Payment) -> Result<()>;
}
