//! The client-tier composite: pricing, pseudosettle, swap, retrieval, and
//! pushsync as one hand-rolled [`ClientBehaviour`] multiplexing headered
//! substreams through a per-connection [`ClientHandler`].
//!
//! These are kept as a single behaviour, not a `#[derive(NetworkBehaviour)]`
//! composition of sub-behaviours, because the client protocols are intended to
//! be unified at the wire level into one protocol; keeping them unified here
//! mirrors that. The handler's substream multiplexing, back-pressure, and the
//! three per-protocol timeouts are load-bearing.
//!
//! The behaviour is accounting-agnostic. It relays a cache miss or a pushsync
//! through the [`Forwarder`] seam; the concrete network forwarder couples to
//! accounting and the outbound client handle and lives in the node crate, which
//! implements this trait against [`StubForwarder`]'s contract.

#![cfg_attr(not(feature = "std"), no_std)]

mod behaviour;
mod events;
mod forward;
mod handler;
mod storer;
pub mod upgrade;

pub use behaviour::{ClientBehaviour, Config as BehaviourConfig};
pub use forward::{
    ForwardError, ForwardedChunk, ForwardedReceipt, Forwarder, StubForwarder, closer_candidates,
};
pub use handler::{ClientHandler, Config as HandlerConfig, HandlerCommand, HandlerEvent};
pub use storer::StorerCapability;
