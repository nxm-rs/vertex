//! Logging configuration for the Vertex Swarm node.

use eyre::Result;
use tracing_subscriber::EnvFilter;
use vertex_node_api::LoggingConfig;

/// Initialize logging based on configuration.
///
/// The filter is built with the following precedence:
/// 1. If logging is disabled, only errors are shown
/// 2. Otherwise, start with `RUST_LOG` env var if set, or default to info level
/// 3. Apply verbosity flags to increase log level
/// 4. Apply any custom filter directive
pub fn init_logging(config: &impl LoggingConfig) -> Result<()> {
    let filter = if !config.logging_enabled() {
        EnvFilter::new("error")
    } else {
        // Start with RUST_LOG env var, or default based on verbosity
        let base_level = match config.verbosity() {
            0 => "info",
            1 => "debug",
            _ => "trace",
        };

        // Try to get from RUST_LOG first, then fall back to our computed level
        let mut filter =
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(base_level));

        // Add any custom filter directives
        if let Some(custom_filter) = config.log_filter() {
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
    if config.logging_enabled() {
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
