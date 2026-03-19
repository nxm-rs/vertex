//! Consolidated protobuf definitions for Swarm network protocols.

#[allow(unreachable_pub, clippy::all)]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/proto/mod.rs"));
}

pub use generated::handshake;
pub use generated::headers;
pub use generated::hive;
pub use generated::pingpong;
pub use generated::pricing;
pub use generated::pseudosettle;
pub use generated::pullsync;
pub use generated::pushsync;
pub use generated::retrieval;
pub use generated::swap;
