//! Logging CLI arguments.

use std::path::PathBuf;

use clap::Args;
use serde::{Deserialize, Serialize};
use vertex_observability::{FileConfig, LogFormat, StdoutConfig};

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
    #[serde(skip)]
    pub verbosity: u8,

    /// Log filter directive (e.g., "vertex=debug,libp2p=info").
    #[arg(long = "log.filter", value_name = "DIRECTIVE")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,

    /// Use JSON format for log output.
    #[arg(long = "log.json")]
    pub json: bool,

    /// Directory to write log files. Enables file logging when set.
    #[arg(long = "log.dir", value_name = "PATH")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_dir: Option<PathBuf>,

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
            log_dir: None,
            max_file_size_mb: DEFAULT_MAX_FILE_SIZE_MB,
            max_files: DEFAULT_MAX_FILES,
        }
    }
}

impl LogArgs {
    /// Build stdout logging config.
    ///
    /// Returns None if quiet mode is enabled.
    pub fn stdout_config(&self) -> Option<StdoutConfig> {
        if self.quiet {
            return None;
        }

        let filter = self.build_filter();
        let format = if self.json {
            LogFormat::Json
        } else {
            LogFormat::Terminal
        };

        Some(StdoutConfig::new(format, filter, true))
    }

    /// Build file logging config from CLI args.
    ///
    /// Returns None if quiet mode is enabled or --log.dir not set.
    pub fn file_config_from_args(&self) -> Option<FileConfig> {
        self.log_dir.as_ref().and_then(|dir| self.file_config(dir.clone()))
    }

    /// Build file logging config with explicit directory.
    ///
    /// Returns None if quiet mode is enabled.
    pub fn file_config(&self, log_dir: PathBuf) -> Option<FileConfig> {
        if self.quiet {
            return None;
        }

        let filter = self.build_filter();
        let format = if self.json {
            LogFormat::Json
        } else {
            LogFormat::Terminal
        };

        Some(FileConfig::new(
            log_dir,
            "vertex.log",
            format,
            filter,
            self.max_file_size_mb,
            self.max_files,
        ))
    }

    fn build_filter(&self) -> String {
        if let Some(ref custom) = self.filter {
            return custom.clone();
        }

        match self.verbosity {
            0 => "info".to_string(),
            1 => "debug".to_string(),
            _ => "trace".to_string(),
        }
    }
}
