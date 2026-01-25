//! CLI argument structs for node infrastructure configuration.
//!
//! These args serve dual purposes:
//! - CLI parsing via clap (`#[derive(Args)]`)
//! - Configuration serialization via serde (`#[derive(Serialize, Deserialize)]`)
//!
//! The structs implement config traits from `vertex_node_api`, allowing them
//! to be passed directly to infrastructure components.
//!
//! See `vertex_swarm_core::args` for the configuration hierarchy documentation.

mod api;
mod database;
mod datadir;
mod log;

pub use api::ApiArgs;
pub use database::DatabaseArgs;
pub use datadir::DataDirArgs;
pub use log::LogArgs;
