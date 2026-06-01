//! Hive protocol for Swarm peer gossip and network bootstrapping.

mod behaviour;
pub mod bzz;
mod codec;
mod error;
mod handler;
pub mod metrics;
mod protocol;
mod verifier;

pub use behaviour::{HiveBehaviour, HiveEvent};
pub use bzz::{BzzAddress, BzzAddressError};
pub use error::ValidationFailure;
pub use verifier::{
    BlocklistQuery, DefaultHiveVerifier, GossipSource, HiveRejection, HiveVerifier, VerifiedPeer,
};

/// Protocol name for hive.
///
/// Bumped from `1.1.0` → `2.0.0` to match bee mainnet: hive 2.0.0 carries the
/// full `BzzAddress` (adds `timestamp` and `chequebook_address` fields) on
/// the wire. See `bee/pkg/hive/pb/hive.proto`.
pub const PROTOCOL_NAME: &str = "/swarm/hive/2.0.0/peers";

/// Maximum number of peers per broadcast message.
pub const MAX_BATCH_SIZE: usize = 30;
