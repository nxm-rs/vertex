//! Client protocol wiring for the Vertex node.
//!
//! The client composite behaviour (pricing, retrieval, pushsync, pseudosettle,
//! and swap behind its feature) lives in `vertex-swarm-client-behaviour` as a
//! single hand-rolled [`ClientBehaviour`]/`ClientHandler` over headered
//! substreams. This module re-exports it under the paths the rest of the node
//! already uses, and keeps the node-local [`NetworkForwarder`]: the concrete
//! relay couples to client accounting and the outbound `ClientHandle`, so it
//! cannot live in the accounting-agnostic behaviour crate.

mod forward;

#[cfg(test)]
mod behaviour_tests;
#[cfg(test)]
mod timeout_repro;

pub(crate) use forward::NetworkForwarder;
pub(crate) use vertex_swarm_client_behaviour::{
    BehaviourConfig, ClientBehaviour, StorerCapability, StubForwarder,
};

#[cfg(feature = "swap")]
pub use vertex_swarm_client_protocol::SwapEvent;
pub use vertex_swarm_client_protocol::{
    ClientCommand, ClientEvent, FailureKind, PseudosettleEvent, PushResponseTx, RetrievalResponseTx,
};
