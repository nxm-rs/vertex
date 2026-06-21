//! Composite libp2p behaviours for the storer node.
//!
//! Currently exposes [`PullsyncBehaviour`], one `NetworkBehaviour` that runs both
//! pullsync substreams: inbound it is the syncer, answering cursor handshakes and
//! range requests from an injected [`PullStorage`](vertex_swarm_api::PullStorage);
//! outbound it is the puller command surface, opening cursor and range substreams
//! on command and emitting their results. The puller service loop (readiness
//! gating, interval persistence, verification, admission) drives this surface from
//! a higher layer.

mod behaviour;
mod error;
mod handler;
pub mod metrics;
mod upgrade;

pub use behaviour::{PullsyncBehaviour, PullsyncEvent};
pub use error::PullsyncFailure;
