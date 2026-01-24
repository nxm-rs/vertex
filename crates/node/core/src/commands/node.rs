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
    cli::{AvailabilityMode, NodeArgs, NodeTypeCli},
    config::{NodeConfig, NodeType},
    dirs::DataDirs,
};
use alloy_signer::k256::ecdsa::SigningKey;
use alloy_signer_local::LocalSigner;
use eyre::{Result, WrapErr};
use std::{fs, path::Path, sync::Arc};
use tracing::{debug, error, info, warn};
use vertex_client_core::SwarmNode;
use vertex_client_kademlia::KademliaTopology;
use vertex_client_peermanager::FilePeerStore;
use vertex_node_identity::SwarmIdentity;
use vertex_node_types::{AnyNodeTypes, Identity};
use vertex_rpc_core::RpcServer;
use vertex_rpc_server::{GrpcServer, GrpcServerConfig};
use vertex_swarm_api::NoAvailabilityIncentives;
use vertex_swarmspec::{init_mainnet, init_testnet, Hive, SwarmSpec};
use vertex_tasks::TaskManager;

/// Default node types for light/publisher clients.
///
/// Uses:
/// - `Hive` spec (mainnet/testnet)
/// - `SwarmIdentity` for signing and overlay address
/// - `KademliaTopology` for peer discovery (built by SwarmNode internally)
/// - `NoAvailabilityIncentives` for now (pseudosettle/SWAP coming)
type DefaultNodeTypes = AnyNodeTypes<
    Hive,                                       // Spec
    SwarmIdentity,                              // Identity
    Arc<KademliaTopology<SwarmIdentity>>,       // Topology
    NoAvailabilityIncentives,                   // Accounting
    (),                                         // Database
    (),                                         // Rpc
    vertex_tasks::TaskExecutor,                 // Executor
>;

/// Run the node command
pub async fn run(args: NodeArgs) -> Result<()> {
    // Create task manager for centralized task lifecycle management
    // This must happen early so TaskExecutor::current() works during build
    let task_manager = TaskManager::current();
    let executor = task_manager.executor();

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

    // Convert CLI node type to config node type
    let node_type = cli_to_node_type(args.node_type);
    info!("Node type: {:?}", node_type);

    // Load or create configuration
    let config_path = dirs.config_file();
    let mut config = NodeConfig::load_or_create(&config_path, &spec, node_type)?;

    // Apply CLI overrides
    config.apply_cli_args(
        &args.network,
        &args.storage,
        &args.storage_incentives,
        &args.api,
        &args.availability,
        node_type,
    );


    // Create peer store for persistence
    let peers_file = config
        .network
        .peers_file
        .as_ref()
        .map(|p| dirs.root.join(p))
        .unwrap_or_else(|| dirs.peers_file());
    let peer_store = Arc::new(
        FilePeerStore::new_with_create_dir(&peers_file)
            .wrap_err_with(|| format!("failed to open peers database: {}", peers_file.display()))?,
    );
    info!("Peers database: {}", peers_file.display());

    // Determine if we need ephemeral or persistent identity
    let use_ephemeral = args.identity.ephemeral || !node_type.requires_persistent_identity();

    // Create identity
    let identity = if use_ephemeral {
        debug!("Creating ephemeral identity");
        SwarmIdentity::random(spec.clone(), !matches!(node_type, NodeType::Light))
    } else {
        // Load or create persistent identity
        let keystore_path = config
            .identity
            .keystore_path
            .as_ref()
            .map(|p| dirs.root.join(p))
            .unwrap_or_else(|| dirs.keys_dir().join("swarm"));

        // Resolve password
        let password = resolve_password(
            args.identity.password.as_deref(),
            args.identity.password_file.as_deref(),
        )?;

        // Load or create signer from keystore
        let signer = load_or_create_signer(&keystore_path, &password)?;

        // Get nonce: CLI arg > config file > generate new
        let (nonce, nonce_source) = if let Some(cli_nonce) = args.identity.nonce {
            (cli_nonce, "CLI argument")
        } else {
            let (n, generated) = config.identity.nonce_or_generate();
            if generated {
                (n, "generated")
            } else {
                (n, "config file")
            }
        };
        debug!("Nonce source: {}", nonce_source);

        SwarmIdentity::new(
            signer,
            nonce,
            spec.clone(),
            !matches!(node_type, NodeType::Light),
        )
    };

    // Save config (may have updated nonce)
    config.save(&config_path)?;
    info!("Configuration file: {}", config_path.display());

    // Log identity configuration first
    info!("Identity configuration:");
    info!("  Ethereum address: {}", identity.ethereum_address());
    info!(
        "  Overlay address: {}",
        hex::encode(identity.overlay_address().as_slice())
    );

    // Log node configuration
    log_node_config(&config, &args.availability.mode);

    // Initialize P2P network using SwarmNode
    info!("Initializing P2P network...");
    let (mut node, client_service, _client_handle) =
        SwarmNode::<DefaultNodeTypes>::builder(identity)
            .with_network_config(&args.network)
            .with_peer_store(peer_store.clone())
            .build()
            .await?;

    // Clone the peer manager for shutdown flushing
    let peer_manager = node.peer_manager().clone();

    info!("Local Peer ID: {}", node.local_peer_id());

    // Start listening for connections
    node.start_listening()?;

    // Connect to bootnodes
    if !spec.bootnodes.is_empty() {
        info!("Connecting to {} bootnode(s)...", spec.bootnodes.len());
        for bn in &spec.bootnodes {
            info!("  Bootnode: {}", bn);
        }

        let connected = node.connect_bootnodes().await?;
        info!("Initiated {} bootnode connection(s)", connected);
    } else {
        warn!("No bootnodes configured - running in isolated mode");
    }

    // Connect to trusted peers if configured
    if let Some(ref trusted_peers) = args.network.trusted_peers {
        let parsed_peers: Vec<libp2p::Multiaddr> = trusted_peers
            .iter()
            .filter_map(|p| match p.parse() {
                Ok(addr) => Some(addr),
                Err(e) => {
                    warn!("Invalid trusted peer multiaddr '{}': {}", p, e);
                    None
                }
            })
            .collect();

        if !parsed_peers.is_empty() {
            info!("Connecting to {} trusted peer(s)...", parsed_peers.len());
            for peer in &parsed_peers {
                info!("  Trusted peer: {}", peer);
                use vertex_net_topology::TopologyCommand;
                node.topology_command(TopologyCommand::Dial(peer.clone()));
            }
        }
    }

    info!("Node initialization complete");

    // Start gRPC server if enabled
    let grpc_server = if config.api.grpc_enabled {
        let grpc_addr = config.grpc_socket_addr();
        info!("Starting gRPC server on {}", grpc_addr);
        let grpc_config = GrpcServerConfig {
            addr: grpc_addr,
            topology_provider: Some(node.kademlia_topology().clone()),
        };
        let server = GrpcServer::with_config(grpc_config);
        Some(server)
    } else {
        None
    };

    // Spawn gRPC server task if enabled (not critical - can be restarted)
    if let Some(server) = grpc_server {
        executor.spawn(async move {
            if let Err(e) = server.start().await {
                error!("gRPC server error: {}", e);
            }
        });
    }

    info!("Starting node... (press Ctrl+C to stop)");

    // Run the client service event loop as a critical task
    executor.spawn_critical("client_service", async move {
        client_service.run().await;
    });

    // Run the node event loop as a critical task
    executor.spawn_critical("swarm_node", async move {
        if let Err(e) = node.run().await {
            error!("Node error: {}", e);
        }
    });

    // Wait for shutdown signal or critical task panic
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("Received shutdown signal");
        }
        result = task_manager => {
            match result {
                Ok(()) => info!("Task manager shutdown cleanly"),
                Err(panic_err) => error!("Critical task panicked: {}", panic_err),
            }
        }
    }

    // Flush peer store before shutdown
    info!("Flushing peer store...");
    match peer_manager.flush() {
        Ok(()) => {
            let stats = peer_manager.stats();
            info!(
                stored = stats.stored_peers,
                connected = stats.connected_peers,
                "Peer store flushed successfully"
            );
        }
        Err(e) => {
            error!("Failed to flush peer store: {}", e);
        }
    }

    info!("Node shutdown complete");
    Ok(())
}

/// Load or create a signer from an Ethereum keystore file.
fn load_or_create_signer(keystore_path: &Path, password: &str) -> Result<LocalSigner<SigningKey>> {
    if keystore_path.exists() {
        info!(
            "Loading signing key from keystore: {}",
            keystore_path.display()
        );
        LocalSigner::decrypt_keystore(keystore_path, password)
            .wrap_err_with(|| format!("Failed to decrypt keystore at {}", keystore_path.display()))
    } else {
        info!("Generating new signing key");

        // Ensure parent directory exists
        if let Some(parent) = keystore_path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Generate new key
        let mut rng = rand::thread_rng();
        let signing_key = SigningKey::random(&mut rng);

        // Get the filename from the path
        let name = keystore_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("swarm");

        // Get the parent directory
        let dir = keystore_path.parent().unwrap_or(Path::new("."));

        // Encrypt and save to keystore
        let (signer, _uuid) = LocalSigner::encrypt_keystore(
            dir,
            &mut rng,
            signing_key.to_bytes().as_slice(),
            password,
            Some(name),
        )
        .wrap_err("Failed to create keystore")?;

        // Set restrictive permissions on Unix
        #[cfg(unix)]
        if keystore_path.exists() {
            use crate::constants::SENSITIVE_FILE_MODE;
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(keystore_path)?.permissions();
            perms.set_mode(SENSITIVE_FILE_MODE);
            fs::set_permissions(keystore_path, perms)?;
        }

        Ok(signer)
    }
}

/// Resolve password from various sources.
///
/// Priority: CLI argument > password file > interactive prompt
pub fn resolve_password(password: Option<&str>, password_file: Option<&Path>) -> Result<String> {
    // Check direct password
    if let Some(pwd) = password {
        return Ok(pwd.to_string());
    }

    // Check password file
    if let Some(path) = password_file {
        let content = fs::read_to_string(path)
            .wrap_err_with(|| format!("Failed to read password file {:?}", path))?;
        return Ok(content.trim().to_string());
    }

    // Try interactive prompt if we're in a terminal
    if atty::is(atty::Stream::Stdin) {
        return rpassword::prompt_password("Enter keystore password: ")
            .wrap_err("Failed to read password from terminal");
    }

    Err(eyre::eyre!(
        "No password provided. Use --password, --password-file, or VERTEX_PASSWORD environment variable"
    ))
}

/// Convert CLI node type to config node type.
fn cli_to_node_type(cli_type: NodeTypeCli) -> NodeType {
    match cli_type {
        NodeTypeCli::Bootnode => NodeType::Bootnode,
        NodeTypeCli::Light => NodeType::Light,
        NodeTypeCli::Publisher => NodeType::Publisher,
        NodeTypeCli::Full => NodeType::Full,
        NodeTypeCli::Staker => NodeType::Staker,
    }
}

/// Resolve which network specification to use based on CLI arguments
fn resolve_network_spec(args: &NodeArgs) -> Result<Arc<Hive>> {
    if args.mainnet {
        Ok(init_mainnet())
    } else if args.testnet {
        Ok(init_testnet())
    } else if let Some(ref path) = args.swarmspec {
        // Load custom SwarmSpec from file
        info!("Loading SwarmSpec from: {}", path.display());
        let spec = Hive::from_file(path)
            .wrap_err_with(|| format!("Failed to load SwarmSpec from {}", path.display()))?;
        Ok(Arc::new(spec))
    } else {
        // Default to mainnet if no network is specified
        warn!("No network specified, defaulting to mainnet");
        Ok(init_mainnet())
    }
}

/// Log the node configuration for debugging
fn log_node_config(config: &NodeConfig, availability_mode: &AvailabilityMode) {
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

    if config.availability.is_some() {
        info!("Availability incentives: {:?}", availability_mode);
    }

    if let Some(ref storage) = config.storage {
        info!("Storage configuration:");
        info!("  Capacity: {} chunks", storage.capacity_chunks);
        info!("  Redistribution: {}", storage.redistribution);
    }

    if config.api.grpc_enabled {
        info!(
            "gRPC server: {}:{}",
            config.api.grpc_addr, config.api.grpc_port
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
    use crate::cli::{
        ApiArgs, AvailabilityArgs, AvailabilityMode, DataDirArgs, IdentityArgs, NetworkArgs,
        NodeTypeCli, StorageArgs, StorageIncentiveArgs,
    };

    fn default_node_args() -> NodeArgs {
        NodeArgs {
            node_type: NodeTypeCli::Light,
            datadir: DataDirArgs { datadir: None },
            network: NetworkArgs {
                disable_discovery: false,
                bootnodes: None,
                trusted_peers: None,
                port: 1634,
                addr: "0.0.0.0".to_string(),
                max_peers: 50,
                idle_timeout_secs: crate::constants::DEFAULT_IDLE_TIMEOUT_SECS,
            },
            availability: AvailabilityArgs {
                mode: AvailabilityMode::Pseudosettle,
                payment_threshold: vertex_bandwidth_core::DEFAULT_PAYMENT_THRESHOLD,
                payment_tolerance_percent: vertex_bandwidth_core::DEFAULT_PAYMENT_TOLERANCE_PERCENT,
                base_price: vertex_bandwidth_core::DEFAULT_BASE_PRICE,
                refresh_rate: vertex_bandwidth_core::DEFAULT_REFRESH_RATE,
                early_payment_percent: vertex_bandwidth_core::DEFAULT_EARLY_PAYMENT_PERCENT,
                light_factor: vertex_bandwidth_core::DEFAULT_LIGHT_FACTOR,
            },
            storage: StorageArgs {
                capacity_chunks: 1_000_000,
                cache_chunks: 100_000,
            },
            storage_incentives: StorageIncentiveArgs {
                redistribution: false,
            },
            api: ApiArgs {
                grpc: false,
                grpc_addr: "127.0.0.1".to_string(),
                grpc_port: 1635,
                metrics: false,
                metrics_addr: "127.0.0.1".to_string(),
                metrics_port: 1637,
            },
            identity: IdentityArgs {
                password: None,
                password_file: None,
                nonce: None,
                ephemeral: true,
            },
            mainnet: false,
            testnet: false,
            swarmspec: None,
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

    #[test]
    fn test_cli_to_node_type() {
        assert_eq!(cli_to_node_type(NodeTypeCli::Bootnode), NodeType::Bootnode);
        assert_eq!(cli_to_node_type(NodeTypeCli::Light), NodeType::Light);
        assert_eq!(
            cli_to_node_type(NodeTypeCli::Publisher),
            NodeType::Publisher
        );
        assert_eq!(cli_to_node_type(NodeTypeCli::Full), NodeType::Full);
        assert_eq!(cli_to_node_type(NodeTypeCli::Staker), NodeType::Staker);
    }
}
