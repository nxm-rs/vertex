//! Database CLI arguments.

use std::path::PathBuf;

use clap::Args;
use serde::{Deserialize, Serialize};

/// Database configuration.
#[derive(Debug, Args, Clone, Default, Serialize, Deserialize)]
#[command(next_help_heading = "Database")]
#[serde(default)]
pub struct DatabaseArgs {
    /// Use in-memory database (no persistence).
    #[arg(long = "db.memory", conflicts_with = "path")]
    pub memory_only: bool,

    /// Database cache size in megabytes.
    #[arg(long = "db.cache", value_name = "MB")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_size_mb: Option<u64>,

    /// Custom database file path (default: <datadir>/db/vertex.redb).
    #[arg(long = "db.path", value_name = "PATH")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
}

/// Resolved database configuration.
#[derive(Debug, Clone)]
pub struct DatabaseConfig {
    /// Path to the database file (None for in-memory).
    pub path: Option<PathBuf>,
    /// Cache size in megabytes.
    pub cache_size_mb: Option<u64>,
}

impl DatabaseArgs {
    /// Build a resolved database configuration.
    ///
    /// If `--db.memory` is set, the path is `None` (in-memory database).
    /// Otherwise, uses `--db.path` or falls back to `default_path`.
    pub fn database_config(&self, default_path: PathBuf) -> DatabaseConfig {
        DatabaseConfig {
            path: if self.memory_only {
                None
            } else {
                Some(self.path.clone().unwrap_or(default_path))
            },
            cache_size_mb: self.cache_size_mb,
        }
    }
}
