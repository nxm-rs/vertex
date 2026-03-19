//! Logging CLI arguments.

use std::io::IsTerminal;

use clap::Args;
use serde::{Deserialize, Serialize};
use vertex_observability::{LogFormat, StdoutConfig};

/// Logging configuration.
#[derive(Debug, Default, Args, Clone, Serialize, Deserialize)]
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

    /// Use JSON format for log output.
    #[arg(long = "log.json")]
    pub json: bool,
}

impl LogArgs {
    /// Build stdout logging config.
    ///
    /// Returns None if quiet mode is enabled. Detects terminal for ANSI colors.
    pub fn stdout_config(&self) -> Option<StdoutConfig> {
        if self.quiet {
            return None;
        }

        let filter = match self.verbosity {
            0 => "info".to_string(),
            1 => "debug".to_string(),
            _ => "trace".to_string(),
        };

        let format = if self.json {
            LogFormat::Json
        } else {
            LogFormat::Terminal
        };

        let ansi = std::io::stdout().is_terminal();

        Some(StdoutConfig::new(format, filter, ansi))
    }
}
