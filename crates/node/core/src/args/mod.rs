//! CLI argument structs for node infrastructure configuration.
//!
//! These args serve dual purposes:
//! - CLI parsing via clap (`#[derive(Args)]`)
//! - Configuration serialization via serde (`#[derive(Serialize, Deserialize)]`)
//!
//! The structs implement config traits from `vertex_node_api`, allowing them
//! to be passed directly to infrastructure components.
//!
//! This module provides two aggregated structs for different use cases:
//!
//! - [`NodeArgs`]: Full infrastructure args including logging (for standalone use)
//! - [`InfraArgs`]: Infrastructure args without logging (for subcommand composition)
//!
//! # Example
//!
//! ```ignore
//! use clap::Parser;
//! use vertex_node_core::args::{LogArgs, InfraArgs};
//! use vertex_swarm_core::args::SwarmArgs;
//!
//! #[derive(Parser)]
//! struct Cli {
//!     #[command(flatten)]
//!     logs: LogArgs,           // Top-level logging
//!
//!     #[command(subcommand)]
//!     command: Commands,
//! }
//!
//! enum Commands {
//!     Node(NodeCommand),
//! }
//!
//! struct NodeCommand {
//!     #[command(flatten)]
//!     infra: InfraArgs,        // Generic infrastructure
//!
//!     #[command(flatten)]
//!     swarm: SwarmArgs,        // Protocol-specific
//! }
//! ```

mod api;
mod database;
mod datadir;
mod log;

pub use api::ApiArgs;
pub use database::DatabaseArgs;
pub use datadir::DataDirArgs;
pub use log::LogArgs;

use clap::Args;
use serde::{Deserialize, Serialize};

/// Infrastructure arguments without logging.
///
/// Use this in CLI subcommands where logging is handled at the top level.
/// For full infrastructure args including logging, use [`NodeArgs`].
#[derive(Debug, Args, Clone, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct InfraArgs {
    /// API configuration (gRPC, metrics).
    #[command(flatten)]
    pub api: ApiArgs,

    /// Database configuration.
    #[command(flatten)]
    pub database: DatabaseArgs,

    /// Data directory configuration.
    #[command(flatten)]
    pub datadir: DataDirArgs,
}


/// Full node infrastructure arguments including logging.
///
/// This struct combines all generic infrastructure CLI arguments including
/// logging. Use [`InfraArgs`] if logging is handled separately at the CLI
/// top level.
#[derive(Debug, Args, Clone, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct NodeArgs {
    /// Logging configuration.
    #[command(flatten)]
    pub logs: LogArgs,

    /// Infrastructure configuration (API, database, data directory).
    #[command(flatten)]
    pub infra: InfraArgs,
}

