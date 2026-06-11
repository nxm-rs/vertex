//! Swarm network topology management.
//!
//! Provides libp2p behaviour, handlers, and Kademlia routing for Swarm peer
//! discovery and connection management.
//!
//! Construction goes through [`TopologyBehaviourBuilder`], which builds the
//! behaviour and its [`TopologyHandle`] without spawning background tasks;
//! [`TopologyBehaviour::spawn_tasks`] starts the connection evaluator,
//! interface watcher, and gossip tasks once a runtime is available.
//!
//! # Timing and capacity assumptions
//!
//! These defaults are not specified by the Book of Swarm; they trade
//! responsiveness against churn and memory. They live in the `behaviour` and
//! `profile` modules and can be overridden through [`TopologyConfig`].
//!
//! - Pacing (connection-evaluation cadence, discovery dial rate, dial
//!   concurrency, bootstrap fill, candidate budgets) is bundled per
//!   [`ConnectionProfile`] and resolved at build time: explicit selection
//!   first, then the network configuration, then the node-type default
//!   (client = aggressive, storer/bootnode = balanced). [`PacingProfile`]
//!   documents the numbers. Discovery dials drain through a GCRA bucket so a
//!   burst of fresh candidates (e.g. after gossip influx) goes out
//!   immediately while the sustained rate stays bounded.
//! - `DEFAULT_EARLY_DISCONNECT_THRESHOLD` (30s) is the floor below which a
//!   post-handshake connection that drops is scored as an early disconnect, so a
//!   peer that repeatedly connects and immediately leaves is penalized.
//! - `DEFAULT_PEER_SAVE_INTERVAL` (300s) bounds how often the known-peer set is
//!   flushed to persistent storage, trading store writes against how many freshly
//!   learned peers a crash can lose.
//! - `EVENT_CHANNEL_CAPACITY` (256) and `COMMAND_CHANNEL_CAPACITY` (64) size the
//!   event-broadcast and command buffers so a burst does not block the poll loop
//!   while staying bounded.
//! - In-flight dials are bounded by the profile's dial concurrency, each
//!   bounded by the handshake timeout; the per-bin routing targets, not this
//!   cap, are the real gate on how many become connections.
//! - Gossip exchange and record-intake tuning (refresh cadence, record
//!   cooldown, per-gossiper budgets) lives in [`GossipConfig`], overridable
//!   through [`TopologyConfig::with_gossip`]. The `gossip` module docs explain
//!   how the limits relate.

#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]

pub(crate) use vertex_net_utils::extract_peer_id;

mod behaviour;
mod builder;
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
mod profile;
mod reachability;
mod readiness;
mod tasks;

#[cfg(test)]
pub(crate) mod test_support;

pub use behaviour::{TopologyBehaviour, TopologyConfig};
pub use builder::TopologyBehaviourBuilder;
pub use error::{DialError, DisconnectReason, RejectionReason, TopologyError, TopologyResult};
pub use events::{ConnectionDirection, DialReason, TopologyCommand, TopologyEvent};
pub use gossip::GossipConfig;
pub use handle::{BinStats, RoutingStats, TopologyHandle};
pub use profile::PacingProfile;

pub use kademlia::{KademliaConfig, RoutingArgs, TopologyPhase};
pub use reachability::{FAILURE_DECAY, FAILURE_THRESHOLD, PeerReachability, ReachabilityTracker};
pub use readiness::{BinReadiness, ReadinessSnapshot};

// Re-exported so consumers configure pacing without extra dependencies.
pub use vertex_net_ratelimiter::Quota;
pub use vertex_swarm_primitives::ConnectionProfile;
