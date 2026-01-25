//! Data directory CLI arguments.

use clap::Args;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Data directory configuration.
#[derive(Debug, Args, Clone, Default, Serialize, Deserialize)]
#[command(next_help_heading = "Datadir")]
#[serde(default)]
pub struct DataDirArgs {
    /// Data directory path for all node data (config, keys, database, logs).
    #[arg(long, value_name = "PATH")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub datadir: Option<PathBuf>,
}
