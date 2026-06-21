//! Pullsync protocol for Swarm storer range synchronisation, over two streams:
//! [`PROTOCOL_CURSORS`] to learn a peer's per-bin cursors, then [`PROTOCOL_SYNC`]
//! per bin to pull missing chunks.
//!
//! Wire invariants: `Ack.cursors[bin]` is the topmost id held for that bin (`0`
//! when empty); in the range exchange `Want` bit `i` selects `chunks[i]` of the
//! preceding `Offer` (MSB-first), answered by one `Delivery` per set bit in offer
//! order.

mod bitvector;
pub use bitvector::{BitVector, BitVectorError};

mod codec;
pub use codec::{Ack, ChunkDescriptor, Delivery, Get, Offer, Syn, Want};

mod error;
pub use error::PullsyncError;

pub mod metrics;

mod protocol;
pub use protocol::{
    CursorsInboundProtocol, CursorsOutboundProtocol, CursorsResponder, SyncInboundProtocol,
    SyncOutboundProtocol, SyncRequester, SyncResponder, cursors_inbound, cursors_outbound,
    sync_inbound, sync_outbound,
};

/// Cursor-handshake stream: `Syn` to `Ack`.
pub const PROTOCOL_CURSORS: &str = "/swarm/pullsync/1.4.0/cursors";

/// Range-exchange stream: `Get` to `Offer` to `Want` to `Delivery`.
pub const PROTOCOL_SYNC: &str = "/swarm/pullsync/1.4.0/pullsync";

/// Maximum chunk descriptors a responder offers in one `Offer` page.
pub const DEFAULT_MAX_PAGE: u64 = 250;

/// Responder rate cap on chunks served per second. Enforced by the behaviour
/// layer, not the codec.
pub const MAX_CHUNKS_PER_SECOND: u64 = 250;

/// Time a responder waits to fill one page before sending it. Enforced by the
/// behaviour layer.
pub const PAGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);
