//! Neighbourhood pull-sync service.
//!
//! The [`Puller`] is the readiness-gated background driver that pulls the
//! storer's neighbourhood into its reserve. It is decoupled from the node swarm
//! loop through the seams in [`seams`]: a [`PullsyncControl`] command surface, a
//! [`PullsyncEvent`] receiver, a [`ReadinessGate`], a [`NeighbourSource`], and a
//! [`ReserveAdmit`] put seam. A later integration step bridges these to the live
//! `PullsyncBehaviour`; this crate wires no node.
//!
//! libp2p enters only through `PeerId` in the command and event surfaces; there
//! is no `Swarm` or `NetworkBehaviour` here.

mod seams;
mod service;
mod verifier;

pub use seams::{
    NeighbourSource, PullsyncControl, PullsyncEvent, ReadinessGate, ReserveAdmit, SyncTarget,
};
pub use service::{
    BuiltPuller, DEFAULT_EVENT_CAPACITY, DEFAULT_PEER_RESPONSE_TIMEOUT, DEFAULT_TAIL_BACKOFF,
    Puller, PullerConfig, PullerHandle, PullerSeams, build_puller, spawn_puller,
};
pub use verifier::SignatureVerifier;
