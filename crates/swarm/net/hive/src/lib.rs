//! Hive: signed peer-record gossip for Swarm topology bootstrapping.
//!
//! Hive lets nodes exchange the [`SwarmPeer`] records they know about so
//! freshly-joined peers can populate their kademlia table without going through
//! a bootnode for every lookup. The protocol is bidirectional: every connection
//! both broadcasts our local view (chunked at [`MAX_BATCH_SIZE`]) and receives
//! the remote's view.
//!
//! [`SwarmPeer`]: vertex_swarm_peer::SwarmPeer
//!
//! # Inbound flow and abuse resistance
//!
//! Every inbound peer record carries a recoverable secp256k1 signature, so
//! validating a batch is `O(n)` ECDSA recoveries. Unbounded ingestion is a
//! cheap DoS: an attacker can stream maximally-sized batches of garbage
//! signatures and burn CPU on every receiver.
//!
//! The reader applies two checks before any cryptography runs:
//!
//! 1. A per-peer GCRA bucket ([`vertex_net_ratelimiter::KeyedRateLimiter`])
//!    charges `len(raw_peers)` tokens against [`HIVE_INBOUND_QUOTA`]. The
//!    cost is on the *raw wire count*, not the post-validation count, so
//!    flooding with invalid signatures does not bypass throttling.
//! 2. Bootnodes (peer handler returns [`InboundPolicy::Discard`]) skip
//!    validation entirely: the records would be dropped anyway. The bootnode
//!    discard is observable via
//!    `hive_peers_discarded_total{reason="bootnode_mode"}` on the raw wire
//!    count.
//!
//! There is no outbound rate limit. The local topology already throttles its
//! own broadcast cadence via dial / save intervals, and the payload is bounded
//! by [`MAX_BATCH_SIZE`]; an outbound per-peer bucket measurably starved
//! legitimate gossip cycles after a few neighborhood refreshes.
//!
//! # Protocol assumptions not in the Book of Swarm
//!
//! - [`HIVE_INBOUND_QUOTA`] is sized off the kademlia bin count (32) with 4x
//!   headroom for the bursts that happen on neighborhood refresh.
//! - [`MAX_BATCH_SIZE`] = 30 peers per wire message, sized so the encoded
//!   protobuf fits comfortably within the framed read buffer alongside the
//!   header exchange.
//! - Wire protocol id [`PROTOCOL_NAME`] = `/swarm/hive/2.0.0/peers`. The
//!   `2.0.0` bump tracks the [`SwarmPeer`] record extension to include
//!   timestamp + chequebook; see the `vertex-swarm-peer` crate for the
//!   sign-data layout.

use std::num::NonZeroU32;
use std::time::Duration;

use vertex_net_ratelimiter::Quota;

mod behaviour;
mod codec;
mod error;
mod handler;
pub mod metrics;
mod peer_handler;
mod protocol;

pub use behaviour::{HiveBehaviour, HiveEvent};
pub use error::ValidationFailure;
pub use peer_handler::{DiscardSilently, HivePeerHandler, InboundPolicy, LearnAndDial};

/// Protocol name for hive.
pub const PROTOCOL_NAME: &str = "/swarm/hive/2.0.0/peers";

/// Maximum number of peers per broadcast message.
pub const MAX_BATCH_SIZE: usize = 30;

/// Per-peer inbound quota for hive batches: 128 token burst, fully replenished
/// every 60 seconds (i.e. one token every ~0.47 s). 128 = 4 * `MaxBins`.
pub const HIVE_INBOUND_QUOTA: Quota =
    Quota::n_every(NonZeroU32::new(128).unwrap(), Duration::from_secs(60));
