//! Logging CLI arguments.

use clap::Args;
use serde::{Deserialize, Serialize};
use vertex_node_api::NodeLoggingConfig;

/// Default log file size in MB.
const DEFAULT_MAX_FILE_SIZE_MB: u64 = 100;

/// Default number of rotated log files to keep.
const DEFAULT_MAX_FILES: usize = 5;

/// Logging configuration.
#[derive(Debug, Args, Clone, Serialize, Deserialize)]
#[command(next_help_heading = "Logging")]
#[serde(default)]
pub struct LogArgs {
    /// Silence all output.
    #[arg(short, long)]
    pub quiet: bool,

    /// Verbose mode (-v, -vv, -vvv, etc.).
    #[arg(short, long, action = clap::ArgAction::Count)]
    #[serde(skip)] // CLI-only, count action doesn't make sense in config
    pub verbosity: u8,

    /// Log filter directive (e.g., "vertex=debug,libp2p=info").
    #[arg(long = "log.filter", value_name = "DIRECTIVE")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,

    /// Use JSON format for log output.
    #[arg(long = "log.json")]
    pub json: bool,

    /// Maximum log file size in megabytes before rotation.
    #[arg(long = "log.max-size", default_value = "100", value_name = "MB")]
    pub max_file_size_mb: u64,

    /// Maximum number of rotated log files to keep.
    #[arg(long = "log.max-files", default_value = "5", value_name = "COUNT")]
    pub max_files: usize,
}

impl Default for LogArgs {
    fn default() -> Self {
        Self {
            quiet: false,
            verbosity: 0,
            filter: None,
            json: false,
            max_file_size_mb: DEFAULT_MAX_FILE_SIZE_MB,
            max_files: DEFAULT_MAX_FILES,
        }
    }
}

impl NodeLoggingConfig for LogArgs {
    fn logging_enabled(&self) -> bool {
        !self.quiet
    }

    fn verbosity(&self) -> u8 {
        self.verbosity
    }

    fn json_logging(&self) -> bool {
        self.json
    }

    fn log_filter(&self) -> Option<&str> {
        self.filter.as_deref()
    }

    fn log_dir(&self) -> Option<&str> {
        // Log directory is derived from the main data directory
        None
    }

    fn max_log_file_size_mb(&self) -> u64 {
        self.max_file_size_mb
    }

    fn max_log_files(&self) -> usize {
        self.max_files
    }
}
