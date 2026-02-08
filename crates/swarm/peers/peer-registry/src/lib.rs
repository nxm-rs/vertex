//! Swarm-specific peer connection registry.

mod reason;
mod registry;

pub use reason::DialReason;
pub use registry::SwarmPeerRegistry;

// Re-export generic types for convenience
pub use vertex_net_peer_registry::{ActivateResult, ConnectionDirection, ConnectionState};
