//! Logging configuration for the Vertex Swarm node.

use crate::cli::LogArgs;
use eyre::Result;
use tracing_subscriber::EnvFilter;

/// Initialize logging based on command line arguments.
///
/// The filter is built with the following precedence:
/// 1. If `--quiet` is set, only errors are shown
/// 2. Otherwise, start with `RUST_LOG` env var if set, or default to info level
/// 3. Apply verbosity flags (-v, -vv, etc.) to increase log level
/// 4. Apply any custom filter from `--log.filter`
pub fn init_logging(args: &LogArgs) -> Result<()> {
    let filter = if args.quiet {
        EnvFilter::new("error")
    } else {
        // Start with RUST_LOG env var, or default based on verbosity
        let base_level = match args.verbosity {
            0 => "info",
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

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .without_time()
        .init();

    // Log startup banner
    if !args.quiet {
        log_startup_banner();
    }

    Ok(())
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
