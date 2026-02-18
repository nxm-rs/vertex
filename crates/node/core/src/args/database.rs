//! Database CLI arguments.

use clap::Args;
use serde::{Deserialize, Serialize};

/// Database configuration.
#[derive(Debug, Args, Clone, Default, Serialize, Deserialize)]
#[command(next_help_heading = "Database")]
#[serde(default)]
pub struct DatabaseArgs {
    /// Use in-memory database (no persistence).
    #[arg(long = "db.memory")]
    pub memory_only: bool,

    /// Database cache size in megabytes.
    #[arg(long = "db.cache", value_name = "MB")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_size_mb: Option<u64>,
}
