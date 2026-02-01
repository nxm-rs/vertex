//! CLI argument structs for Swarm client configuration.

mod network;
mod storage;
mod swarm;

pub use network::NetworkArgs;
pub use storage::StorageIncentiveArgs;
pub use swarm::{NodeTypeArg, ProtocolArgs};
