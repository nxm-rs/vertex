//! Node command - Run a Swarm node
//!
//! This command starts a Swarm node with the specified configuration.
//! It handles:
//!
//! - Loading or creating the node configuration
//! - Initializing the appropriate SwarmSpec (mainnet, testnet, or custom)
//! - Loading or creating node identity (wallet + nonce)
//! - Setting up the P2P network and connecting to bootnodes
//! - Running the node until shutdown is requested

use crate::{
    cli::NodeArgs,
    config::{NodeConfig, NodeMode},
    dirs::DataDirs,
    identity_manager::{IdentityConfig, IdentityManager, resolve_password},
    network::{Network, NetworkConfig},
};
use eyre::Result;
use std::sync::Arc;
use tracing::{info, warn};
use vertex_swarmspec::{init_mainnet, init_testnet, Hive, SwarmSpec};

/// Run the node command
pub async fn run(args: NodeArgs) -> Result<()> {
    // Determine which network to connect to
    let spec = resolve_network_spec(&args)?;
    info!(
        "Network: {} (ID: {})",
        spec.network_name(),
        spec.network_id()
    );

    // Initialize data directories
    let dirs = DataDirs::new(&spec, &args.datadir)?;
    info!("Data directory: {}", dirs.root.display());

    // Determine node mode
    let mode = if args.light {
        NodeMode::Light
    } else if args.storage.redistribution {
        NodeMode::Incentivized
    } else {
        NodeMode::Full
    };

    info!("Node mode: {:?}", mode);

    // Load or create configuration
    let config_path = dirs.config_file();
    let mut config = NodeConfig::load_or_create(&config_path, &spec, mode)?;

    // Apply CLI overrides
    config.apply_cli_args(&args.network, &args.storage, &args.api, args.light);

    info!("Configuration loaded from: {}", config_path.display());

    // Log network configuration
    log_network_config(&config, &spec);

    // Create network configuration from swarmspec bootnodes
    let network_config = NetworkConfig {
        listen_addrs: vec![
            format!("/ip4/{}/tcp/{}", config.network.addr, config.network.port)
                .parse()
                .unwrap(),
        ],
        bootnodes: spec.bootnodes.clone(),
        ..Default::default()
    };

    // Build identity configuration
    let identity_config = IdentityConfig {
        ephemeral: args.identity.ephemeral,
        redistribution: args.storage.redistribution,
        swap_enabled: config.bandwidth.swap_enabled,
        staking: args.storage.staking,
    };

    // Resolve password if needed for persistent identity
    let password = if identity_config.requires_persistent() && !identity_config.ephemeral {
        resolve_password(
            args.identity.password.as_deref(),
            args.identity.password_file.as_deref(),
        )?
    } else {
        String::new()
    };

    // Create identity manager based on mode
    let identity_manager = if identity_config.ephemeral || args.light {
        IdentityManager::ephemeral(dirs.state_dir())
    } else {
        IdentityManager::with_file_keystore(dirs.keys_dir(), dirs.state_dir())
    };

    // Load or create identity
    let identity = identity_manager.load_or_create(
        spec.network_id(),
        mode,
        &identity_config,
        &password,
    )?;

    info!("Ethereum address: {}", identity.ethereum_address());
    info!("Overlay address: {}", hex::encode(identity.overlay_address().as_slice()));

    // Initialize P2P network
    info!("Initializing P2P network...");
    let mut network = Network::new(network_config, identity).await?;

    info!("Local Peer ID: {}", network.local_peer_id());

    // Start listening for connections
    network.start_listening()?;

    // Connect to bootnodes
    if !spec.bootnodes.is_empty() {
        info!("Connecting to {} bootnode(s)...", spec.bootnodes.len());
        for bn in &spec.bootnodes {
            info!("  Bootnode: {}", bn);
        }

        let connected = network.connect_bootnodes().await?;
        info!("Initiated {} bootnode connection(s)", connected);
    } else {
        warn!("No bootnodes configured - running in isolated mode");
    }

    info!("Node initialization complete");
    info!("Starting node... (press Ctrl+C to stop)");

    // Run the network event loop in the background
    let network_handle = tokio::spawn(async move {
        if let Err(e) = network.run().await {
            tracing::error!("Network error: {}", e);
        }
    });

    // Wait for shutdown signal
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("Received shutdown signal");
        }
        _ = network_handle => {
            warn!("Network task ended unexpectedly");
        }
    }

    info!("Node shutdown complete");
    Ok(())
}

/// Resolve which network specification to use based on CLI arguments
fn resolve_network_spec(args: &NodeArgs) -> Result<Arc<Hive>> {
    if args.mainnet {
        Ok(init_mainnet())
    } else if args.testnet {
        Ok(init_testnet())
    } else {
        // Default to mainnet if no network is specified
        // In the future, we might want to default to testnet or require explicit selection
        warn!("No network specified, defaulting to mainnet");
        Ok(init_mainnet())
    }
}

/// Log the network configuration for debugging
fn log_network_config(config: &NodeConfig, _spec: &Hive) {
    info!("Network configuration:");
    info!("  Discovery: {}", config.network.discovery);
    info!("  Max peers: {}", config.network.max_peers);
    info!(
        "  Listen address: {}:{}",
        config.network.addr, config.network.port
    );

    if config.network.bootnodes.is_empty() {
        info!("  Bootnodes: using defaults from swarmspec");
    } else {
        info!(
            "  Bootnodes: {} custom nodes",
            config.network.bootnodes.len()
        );
    }

    info!("Storage configuration:");
    info!(
        "  Max storage: {} GB",
        config.storage.max_storage / (1024 * 1024 * 1024)
    );
    info!("  Max chunks: {}", config.storage.max_chunks);
    info!("  Redistribution: {}", config.storage.redistribution);

    if config.api.http_enabled {
        info!(
            "HTTP API: {}:{}",
            config.api.http_addr, config.api.http_port
        );
    }

    if config.api.metrics_enabled {
        info!(
            "Metrics: {}:{}",
            config.api.metrics_addr, config.api.metrics_port
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{ApiArgs, DataDirArgs, IdentityArgs, NetworkArgs, StorageArgs};

    fn default_node_args() -> NodeArgs {
        NodeArgs {
            datadir: DataDirArgs { datadir: None },
            network: NetworkArgs {
                disable_discovery: false,
                bootnodes: None,
                port: 1634,
                addr: "0.0.0.0".to_string(),
                max_peers: 50,
            },
            storage: StorageArgs {
                capacity: 100,
                redistribution: false,
                staking: false,
            },
            api: ApiArgs {
                http: false,
                http_addr: "127.0.0.1".to_string(),
                http_port: 1633,
                metrics: false,
                metrics_addr: "127.0.0.1".to_string(),
                metrics_port: 1636,
            },
            identity: IdentityArgs {
                password: None,
                password_file: None,
                ephemeral: true,
            },
            light: false,
            mainnet: false,
            testnet: false,
        }
    }

    #[test]
    fn test_resolve_mainnet() {
        let mut args = default_node_args();
        args.mainnet = true;

        let spec = resolve_network_spec(&args).unwrap();
        assert!(spec.is_mainnet());
    }

    #[test]
    fn test_resolve_testnet() {
        let mut args = default_node_args();
        args.testnet = true;

        let spec = resolve_network_spec(&args).unwrap();
        assert!(spec.is_testnet());
    }

    #[test]
    fn test_resolve_default_to_mainnet() {
        let args = default_node_args();

        let spec = resolve_network_spec(&args).unwrap();
        assert!(spec.is_mainnet());
    }
}
