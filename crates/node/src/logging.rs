//! Logging configuration for the Vertex Swarm node.

use crate::{cli::LogArgs, dirs};
use eyre::Result;
use std::{
    fs::File,
    io::{self, Write},
    path::Path,
};
use tracing_appender::non_blocking::{NonBlocking, WorkerGuard};
use tracing_subscriber::{
    fmt::{self, format::FmtSpan},
    prelude::*,
    EnvFilter,
};

/// Initialize logging based on command line arguments.
pub fn init_logging(args: &LogArgs) -> Result<Option<WorkerGuard>> {
    let filter = if args.quiet {
        EnvFilter::new("error")
    } else {
        let level = match args.verbosity {
            0 => "error",
            1 => "warn",
            2 => "info",
            3 => "debug",
            _ => "trace",
        };

        let filter_str = match &args.filter {
            Some(filter) => format!("{},{}", level, filter),
            None => level.to_string(),
        };

        EnvFilter::try_new(filter_str)?
    };

    let mut builder = tracing_subscriber::fmt::Subscriber::builder()
        .with_env_filter(filter)
        .with_span_events(FmtSpan::CLOSE)
        .with_writer(io::stdout);

    if !args.timestamps {
        builder = builder.without_time();
    }

    // Set up file logging if enabled
    let guard = if args.log_file {
        let log_dir = args.log_dir.clone().unwrap_or_else(|| {
            dirs::default_logs_dir().unwrap_or_else(|| Path::new("logs").to_path_buf())
        });

        std::fs::create_dir_all(&log_dir)?;

        let file_appender = tracing_appender::rolling::daily(&log_dir, "vertex.log");
        let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

        builder = builder.with_writer(non_blocking);
        Some(guard)
    } else {
        None
    };

    // Initialize the subscriber
    builder.init();

    // Log startup banner
    if !args.quiet {
        log_startup_banner();
    }

    Ok(guard)
}

/// Log a startup banner with the Vertex Swarm logo and version
fn log_startup_banner() {
    let banner = format!(
        r#"
 _   _          _
| | | |        | |
| | | | ___ _ __| |_ _____  __
| | | |/ _ \ '__| __/ _ \ \/ /
\ \_/ /  __/ |  | ||  __/>  <
 \___/ \___|_|   \__\___/_/\_\

 Swarm Node v{}
    "#,
        crate::version::SHORT_VERSION
    );

    println!("{}", banner);
}

/// Create a logger for a specific file that manages its own file handle
pub fn file_logger(path: impl AsRef<Path>) -> Result<impl Write> {
    let path = path.as_ref();

    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let file = File::create(path)?;
    Ok(file)
}

/// Create a non-blocking file logger
pub fn non_blocking_file_logger(path: impl AsRef<Path>) -> Result<(NonBlocking, WorkerGuard)> {
    let path = path.as_ref();

    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let file = File::create(path)?;
    let (non_blocking, guard) = tracing_appender::non_blocking(file);
    Ok((non_blocking, guard))
}
