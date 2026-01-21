//! Logging configuration for the Vertex Swarm node.

use crate::{cli::LogArgs, dirs};
use eyre::Result;
use std::{fs::File, io::Write, path::Path};
use tracing_appender::non_blocking::{NonBlocking, WorkerGuard};
use tracing_subscriber::{fmt::format::FmtSpan, EnvFilter};

/// Initialize logging based on command line arguments.
///
/// The filter is built with the following precedence:
/// 1. If `--quiet` is set, only errors are shown
/// 2. Otherwise, start with `RUST_LOG` env var if set, or default to info level
/// 3. Apply verbosity flags (-v, -vv, etc.) to increase log level
/// 4. Apply any custom filter from `--filter`
pub fn init_logging(args: &LogArgs) -> Result<Option<WorkerGuard>> {
    let filter = if args.quiet {
        EnvFilter::new("error")
    } else {
        // Start with RUST_LOG env var, or default based on verbosity
        let base_level = match args.verbosity {
            0 => "info",  // Default to info instead of error
            1 => "debug",
            _ => "trace",
        };

        // Try to get from RUST_LOG first, then fall back to our computed level
        let mut filter = EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new(base_level));

        // Add any custom filter directives
        if let Some(custom_filter) = &args.filter {
            for directive in custom_filter.split(',') {
                if let Ok(d) = directive.parse() {
                    filter = filter.add_directive(d);
                }
            }
        }

        filter
    };

    // Set up file logging if enabled
    let guard = if args.log_file {
        let log_dir = args.log_dir.clone().unwrap_or_else(|| {
            dirs::default_logs_dir().unwrap_or_else(|| Path::new("logs").to_path_buf())
        });

        std::fs::create_dir_all(&log_dir)?;

        let file_appender = tracing_appender::rolling::daily(&log_dir, "vertex.log");
        let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

        // Build with file writer
        if args.timestamps {
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_span_events(FmtSpan::CLOSE)
                .with_writer(non_blocking)
                .init();
        } else {
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_span_events(FmtSpan::CLOSE)
                .with_writer(non_blocking)
                .without_time()
                .init();
        }

        Some(guard)
    } else {
        // Build with stdout writer
        if args.timestamps {
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_span_events(FmtSpan::CLOSE)
                .init();
        } else {
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_span_events(FmtSpan::CLOSE)
                .without_time()
                .init();
        }

        None
    };

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
