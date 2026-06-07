//! Swarm network topology management.
//!
//! Provides libp2p behaviour, handlers, and Kademlia routing for Swarm peer
//! discovery and connection management.
//!
//! # Timing and capacity assumptions
//!
//! These defaults are not specified by the Book of Swarm; they trade
//! responsiveness against churn and memory. They live in the `behaviour` module
//! and can be overridden through [`TopologyConfig`].
//!
//! - [`DEFAULT_DIAL_INTERVAL`] (5s) is the cadence of the connection-evaluation
//!   loop: how often the behaviour reconsiders which bins are under target and
//!   issues new dials. Shorter wastes work on a stable table; longer slows
//!   convergence after churn.
//! - `DEFAULT_EARLY_DISCONNECT_THRESHOLD` (30s) is the floor below which a
//!   post-handshake connection that drops is scored as an early disconnect, so a
//!   peer that repeatedly connects and immediately leaves is penalized.
//! - `DEFAULT_PEER_SAVE_INTERVAL` (300s) bounds how often the known-peer set is
//!   flushed to persistent storage, trading store writes against how many freshly
//!   learned peers a crash can lose.
//! - `EVENT_CHANNEL_CAPACITY` (256) and `COMMAND_CHANNEL_CAPACITY` (64) size the
//!   event-broadcast and command buffers so a burst does not block the poll loop
//!   while staying bounded.
//! - The dialer tracks at most 256 in-flight dials, each bounded by the
//!   handshake timeout; the per-bin routing targets, not this cap, are the real
//!   gate on how many become connections.

#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]

pub(crate) use vertex_net_utils::extract_peer_id;

mod behaviour;
mod connection_handlers;
mod dialing;
mod events;
mod handle;
mod kademlia;
pub mod metrics;
mod nat_discovery;
mod protocol_handlers;

mod composed;
mod error;
mod gossip;
mod reachability;
mod tasks;

#[cfg(test)]
pub(crate) mod test_support;

pub use behaviour::{DEFAULT_DIAL_INTERVAL, TopologyBehaviour, TopologyConfig};
pub use error::{DialError, DisconnectReason, RejectionReason, TopologyError, TopologyResult};
pub use events::{ConnectionDirection, DialReason, TopologyCommand, TopologyEvent};
pub use handle::{BinStats, RoutingStats, TopologyHandle};

pub use kademlia::{KademliaConfig, RoutingArgs};
pub use reachability::{FAILURE_DECAY, FAILURE_THRESHOLD, PeerReachability, ReachabilityTracker};
