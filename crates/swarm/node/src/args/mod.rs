//! CLI argument structs and validated configurations for Swarm.
//!
//! The argument structs need the `cli` feature; the validated configs
//! ([`NetworkConfig`], [`PeerConfig`], [`SwapConfig`]) compile in every build so
//! the client core can carry them whole.

#[cfg(feature = "cli")]
mod chain;
mod network;
mod peer;
#[cfg(feature = "cli")]
mod spec;
mod swap;
#[cfg(feature = "cli")]
mod swarm;

#[cfg(feature = "cli")]
pub use chain::{ChainArgs, ChainConfig};
#[cfg(feature = "cli")]
pub use network::NetworkArgs;
pub use network::NetworkConfig;
#[cfg(feature = "cli")]
pub use peer::PeerArgs;
pub use peer::PeerConfig;
#[cfg(feature = "cli")]
pub use spec::SwarmSpecArgs;
#[cfg(feature = "cli")]
pub use swap::SwapArgs;
pub use swap::SwapConfig;
#[cfg(feature = "cli")]
pub use swarm::{NodeTypeArg, ProtocolArgs};
#[cfg(feature = "cli")]
pub use vertex_swarm_topology::RoutingArgs;
