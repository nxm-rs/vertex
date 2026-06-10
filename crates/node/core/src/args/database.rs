//! Database CLI arguments.
//!
//! The database is in-memory by default. Persistence is opt-in: `--db.path`
//! selects an explicit database file, while `--db.persist` uses the
//! conventional default location under the network data directory.

use std::path::PathBuf;

use clap::Args;
use serde::{Deserialize, Serialize};

/// Database configuration.
#[derive(Debug, Args, Clone, Default, Serialize, Deserialize)]
#[command(next_help_heading = "Database")]
#[serde(default)]
pub struct DatabaseArgs {
    /// Persist the database at the default location (<datadir>/<network>/db/vertex.redb).
    #[arg(long = "db.persist")]
    pub persist: bool,

    /// Persist the database at a custom file path (implies --db.persist).
    #[arg(long = "db.path", value_name = "PATH")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,

    /// Database cache size in megabytes.
    #[arg(long = "db.cache", value_name = "MB")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_size_mb: Option<u64>,
}

/// Resolved database configuration.
#[derive(Debug, Clone, Default)]
pub struct DatabaseConfig {
    /// Path to the database file (None for in-memory).
    pub path: Option<PathBuf>,
    /// Cache size in megabytes.
    pub cache_size_mb: Option<u64>,
}

impl DatabaseArgs {
    /// Build a resolved database configuration.
    ///
    /// `--db.path` takes precedence. Otherwise `--db.persist` selects
    /// `default_path`. With neither flag the path is `None` and the
    /// database is in-memory.
    pub fn database_config(&self, default_path: PathBuf) -> DatabaseConfig {
        DatabaseConfig {
            path: self
                .path
                .clone()
                .or_else(|| self.persist.then_some(default_path)),
            cache_size_mb: self.cache_size_mb,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        database: DatabaseArgs,
    }

    fn default_path() -> PathBuf {
        PathBuf::from("/data/network/db/vertex.redb")
    }

    #[test]
    fn no_flags_resolves_in_memory() {
        let cli = TestCli::try_parse_from(["test"]).expect("default should parse");
        let config = cli.database.database_config(default_path());
        assert_eq!(config.path, None, "no flags means in-memory");
    }

    #[test]
    fn persist_flag_resolves_default_path() {
        let cli = TestCli::try_parse_from(["test", "--db.persist"]).expect("flag should parse");
        let config = cli.database.database_config(default_path());
        assert_eq!(config.path, Some(default_path()));
    }

    #[test]
    fn path_flag_resolves_custom_path() {
        let cli = TestCli::try_parse_from(["test", "--db.path", "/custom/db.redb"])
            .expect("flag should parse");
        let config = cli.database.database_config(default_path());
        assert_eq!(config.path, Some(PathBuf::from("/custom/db.redb")));
    }

    #[test]
    fn path_flag_wins_over_persist() {
        let cli = TestCli::try_parse_from(["test", "--db.persist", "--db.path", "/custom/db.redb"])
            .expect("flags should parse");
        let config = cli.database.database_config(default_path());
        assert_eq!(config.path, Some(PathBuf::from("/custom/db.redb")));
    }

    #[test]
    fn cache_size_carries_through() {
        let cli = TestCli::try_parse_from(["test", "--db.cache", "256"]).expect("should parse");
        let config = cli.database.database_config(default_path());
        assert_eq!(config.cache_size_mb, Some(256));
    }
}
