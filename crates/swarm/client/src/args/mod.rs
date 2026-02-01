//! CLI argument structs for Swarm client configuration.
//!
//! These args serve dual purposes:
//! - CLI parsing via clap (`#[derive(Args)]`)
//! - Configuration serialization via serde (`#[derive(Serialize, Deserialize)]`)
//!
//! The structs implement config traits from `vertex_swarm_api`, allowing them
//! to be passed directly to component builders.

mod network;

pub use network::NetworkArgs;
