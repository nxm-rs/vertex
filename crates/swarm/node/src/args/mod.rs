//! CLI argument structs for Swarm client configuration.

mod network;
mod swarm;

pub use network::NetworkArgs;
pub use swarm::{NodeTypeArg, ProtocolArgs};
