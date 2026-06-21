//! Re-exports of the client command and event contract.
//!
//! The definitions live in `vertex-swarm-client-protocol` so the settlement
//! services can share them without depending up on this crate.

pub(crate) use vertex_swarm_client_protocol::{FailureKind, PushResponseTx, RetrievalResponseTx};
