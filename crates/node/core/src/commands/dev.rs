//! Dev command - Run a development Swarm node
//!
//! This command starts a development node with simplified configuration
//! suitable for local testing and development. It features:
//!
//! - Isolated dev network (no connection to mainnet/testnet)
//! - Pre-funded test accounts
//! - Instant or configurable block time
//! - In-memory or local storage

use crate::{
    cli::DevArgs,
    config::{NodeConfig, NodeMode},
    dirs::DataDirs,
    node::VertexNodeBuilder,
};
use eyre::Result;
use std::sync::Arc;
use tracing::info;
use vertex_swarmspec::{init_dev, Hive, SwarmSpec};

/// Run the dev command
pub async fn run(args: DevArgs) -> Result<()> {
    // Create a dev network specification
    let spec = create_dev_spec(&args)?;
    info!(
        "Development network: {} (ID: {})",
        spec.network_name(),
        spec.network_id()
    );

    // Initialize data directories
    let dirs = DataDirs::new(&spec, &args.datadir)?;
    info!("Data directory: {}", dirs.root.display());

    // Create configuration for dev mode
    let config_path = dirs.config_file();
    let mut config = NodeConfig::load_or_create(&config_path, &spec, NodeMode::Full)?;

    // Apply dev-specific settings
    config.network.discovery = false; // No discovery in dev mode
    config.network.bootnodes = Vec::new(); // No bootnodes
    config.bandwidth.pseudosettle_enabled = true;
    config.bandwidth.swap_enabled = false;

    // Apply API args
    config.api.http_enabled = args.api.http;
    config.api.http_addr = args.api.http_addr.clone();
    config.api.http_port = args.api.http_port;
    config.api.metrics_enabled = args.api.metrics;

    info!("Development mode configuration:");
    info!("  Block time: {} seconds", args.block_time);
    info!("  Test accounts: {}", args.accounts);
    info!("  Prefund amount: {} BZZ", args.prefund_amount);

    // TODO: Generate test accounts
    // For now, we just log that we would generate them
    info!("Generating {} test accounts...", args.accounts);
    for i in 0..args.accounts {
        info!(
            "  Account {}: 0x{:040x} - {} BZZ",
            i,
            i as u64 * 12345, // Placeholder address
            args.prefund_amount
        );
    }

    // Initialize node components using the NodeTypes pattern
    info!("Initializing dev node components...");
    let _node = VertexNodeBuilder::new().build();

    info!("Dev node components initialized:");

    info!("Node initialization complete");

    // Start the dev node
    info!("Starting development node... (press Ctrl+C to stop)");

    if args.block_time == 0 {
        info!("Instant mining enabled");
    } else {
        info!("Block time: {} seconds", args.block_time);
    }

    // Wait for shutdown signal
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("Received shutdown signal");
        }
    }

    info!("Development node shutdown complete");
    Ok(())
}

/// Create a development network specification
fn create_dev_spec(_args: &DevArgs) -> Result<Arc<Hive>> {
    // Use the default dev spec
    let spec = init_dev();

    // In the future, we could customize the dev spec based on args
    // For example:
    // - Custom genesis timestamp
    // - Custom network ID to isolate multiple dev nodes

    Ok(spec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{ApiArgs, DataDirArgs};

    fn default_dev_args() -> DevArgs {
        DevArgs {
            datadir: DataDirArgs {
                datadir: None,
                static_files_path: None,
            },
            api: ApiArgs {
                http: true,
                http_addr: "127.0.0.1".to_string(),
                http_port: 1633,
                metrics: false,
                metrics_addr: "127.0.0.1".to_string(),
                metrics_port: 1636,
                cors: None,
                auth: false,
            },
            block_time: 0,
            accounts: 10,
            prefund_amount: 1000,
        }
    }

    #[test]
    fn test_create_dev_spec() {
        let args = default_dev_args();
        let spec = create_dev_spec(&args).unwrap();

        assert!(spec.is_dev());
        assert!(!spec.is_mainnet());
        assert!(!spec.is_testnet());
    }
}
