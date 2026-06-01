//! Core abstractions for Swarm wire protocols.
//!
//! This crate defines the [`SwarmProtocol`] trait — a single shape every Swarm
//! protocol implements so the rest of the stack (handlers, behaviours, dialers)
//! can speak about protocols generically instead of with ad-hoc per-protocol
//! constants. It also exposes [`SemanticVersion`], the version newtype used in
//! Swarm protocol IDs, and the [`swarm_protocol_id!`] macro that constructs the
//! canonical `/swarm/{name}/{version}/{stream}` string at compile time.
//!
//! The concrete codecs and message types continue to live in their respective
//! protocol crates; this crate only fixes the *shape* they must expose.

mod protocol;
mod version;

pub use protocol::{ProtoCodec, SwarmProtocol};
pub use version::SemanticVersion;
