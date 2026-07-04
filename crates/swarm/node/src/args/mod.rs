//! CLI argument structs and validated configurations for Swarm.
//!
//! The argument structs need the `cli` feature; the validated [`SwapConfig`]
//! compiles in every build so the client core can carry it whole.

#[cfg(feature = "cli")]
mod chain;
#[cfg(feature = "cli")]
mod network;
#[cfg(feature = "cli")]
mod peer;
#[cfg(feature = "cli")]
mod spec;
mod swap;
#[cfg(feature = "cli")]
mod swarm;

#[cfg(feature = "cli")]
pub use chain::{ChainArgs, ChainConfig};
#[cfg(feature = "cli")]
pub use network::{NetworkArgs, NetworkConfig};
#[cfg(feature = "cli")]
pub use peer::{PeerArgs, PeerConfig};
#[cfg(feature = "cli")]
pub use spec::SwarmSpecArgs;
#[cfg(feature = "cli")]
pub use swap::SwapArgs;
pub use swap::SwapConfig;
#[cfg(feature = "cli")]
pub use swarm::{NodeTypeArg, ProtocolArgs};
#[cfg(feature = "cli")]
pub use vertex_swarm_topology::RoutingArgs;
