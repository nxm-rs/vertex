//! Node command - Run a Swarm node
//!
//! This command starts a Swarm node with the specified configuration.
//! It handles:
//!
//! - Loading configuration from defaults, env, config file, and CLI
//! - Initializing the appropriate SwarmSpec (mainnet, testnet, or custom)
//! - Loading or creating node identity (wallet + nonce)
//! - Setting up the P2P network and connecting to bootnodes
//! - Running the node until shutdown is requested

use crate::cli::NodeArgs;
use crate::config::NodeConfig;
use vertex_swarm_core::SwarmNodeType;
use alloy_primitives::B256;
use alloy_signer::k256::ecdsa::SigningKey;
use alloy_signer_local::LocalSigner;
use eyre::{Result, WrapErr};
use rand::RngCore;
use std::{fs, path::Path, sync::Arc};
use tracing::{debug, error, info, warn};
use vertex_client_core::SwarmNode;
use vertex_client_kademlia::KademliaTopology;
use vertex_client_peermanager::FilePeerStore;
use vertex_node_core::dirs::DataDirs;
use vertex_node_identity::SwarmIdentity;
use vertex_node_types::{AnyNodeTypes, Identity};
use vertex_node_api::RpcConfig;
use vertex_rpc_core::RpcServer;
use vertex_rpc_server::{GrpcServer, GrpcServerConfig};
use vertex_swarm_api::NoAvailabilityIncentives;
use vertex_swarmspec::{Hive, SwarmSpec, init_mainnet, init_testnet};
use vertex_tasks::TaskManager;

/// Default node types for light/publisher clients.
///
/// Uses:
/// - `Hive` spec (mainnet/testnet)
/// - `SwarmIdentity` for signing and overlay address
/// - `KademliaTopology` for peer discovery (built by SwarmNode internally)
/// - `NoAvailabilityIncentives` for now (pseudosettle/SWAP coming)
type DefaultNodeTypes = AnyNodeTypes<
    Hive,                                 // Spec
    SwarmIdentity,                        // Identity
    Arc<KademliaTopology<SwarmIdentity>>, // Topology
    NoAvailabilityIncentives,             // Accounting
    (),                                   // Database
    (),                                   // Rpc
    vertex_tasks::TaskExecutor,           // Executor
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

    // Load configuration (defaults < env < config file)
    let config_path = dirs.config_file();
    let mut config = NodeConfig::load(if config_path.exists() {
        Some(config_path.as_path())
    } else {
        None
    })?;

    // Apply CLI overrides (CLI has highest priority)
    config.node_type = args.node_type;
    config.network = args.network.clone();
    config.availability = args.availability.clone();
    config.storage = args.storage.clone();
    config.storage_incentives = args.storage_incentives.clone();
    config.api = args.api.clone();
    config.identity = args.identity.clone();
    config.database = args.database.clone();

    let node_type = config.node_type;
    info!("Node type: {:?}", node_type);

    // Create peer store for persistence
    let peers_file = dirs.peers_file();
    let peer_store = Arc::new(
        FilePeerStore::new_with_create_dir(&peers_file)
            .wrap_err_with(|| format!("failed to open peers database: {}", peers_file.display()))?,
    );
    info!("Peers database: {}", peers_file.display());

    // Determine if we need ephemeral or persistent identity
    let use_ephemeral = config.identity.ephemeral || !node_type.requires_persistent_identity();

    // Create identity
    let identity = if use_ephemeral {
        debug!("Creating ephemeral identity");
        SwarmIdentity::random(spec.clone(), !matches!(node_type, SwarmNodeType::Light))
    } else {
        // Load or create persistent identity
        let keystore_path = dirs.keys_dir().join("swarm");

        // Resolve password
        let password = resolve_password(
            config.identity.password.as_deref(),
            config.identity.password_file.as_deref(),
        )?;

        // Load or create signer from keystore
        let signer = load_or_create_signer(&keystore_path, &password)?;

        // Get nonce from config (may be from CLI, env, config file, or generated)
        let nonce = config.identity.nonce.unwrap_or_else(|| {
            let nonce = generate_random_nonce();
            debug!("Generated new nonce");
            nonce
        });

        SwarmIdentity::new(
            signer,
            nonce,
            spec.clone(),
            !matches!(node_type, SwarmNodeType::Light),
        )
    };

    // Log identity configuration
    info!("Identity configuration:");
    info!("  Ethereum address: {}", identity.ethereum_address());
    info!(
        "  Overlay address: {}",
        hex::encode(identity.overlay_address().as_slice())
    );

    // Log node configuration
    log_node_config(&config);

    // Initialize P2P network using SwarmNode
    info!("Initializing P2P network...");
    let (mut node, client_service, _client_handle) =
        SwarmNode::<DefaultNodeTypes>::builder(identity)
            .with_network_config(&config.network)
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
    if let Some(ref trusted_peers) = config.network.trusted_peers {
        if !trusted_peers.is_empty() {
            info!("Connecting to {} trusted peer(s)...", trusted_peers.len());
            for peer in trusted_peers {
                info!("  Trusted peer: {}", peer);
            }
            let dialed = node.dial_addresses(trusted_peers);
            info!("Initiated {} trusted peer connection(s)", dialed);
        }
    }

    info!("Node initialization complete");

    // Start gRPC server if enabled
    let grpc_server = if config.api.grpc_enabled() {
        let grpc_config = GrpcServerConfig::from_config(&config.api)
            .with_topology(node.kademlia_topology().clone());
        info!("Starting gRPC server on {}", grpc_config.addr);
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
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(keystore_path)?.permissions();
            perms.set_mode(0o600);
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

/// Generate a random 32-byte nonce.
fn generate_random_nonce() -> B256 {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    B256::from(bytes)
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
fn log_node_config(config: &NodeConfig) {
    info!("Network configuration:");
    info!("  Discovery: {}", !config.network.disable_discovery);
    info!("  Max peers: {}", config.network.max_peers);
    info!(
        "  Listen address: {}:{}",
        config.network.addr, config.network.port
    );

    if let Some(ref bootnodes) = config.network.bootnodes {
        if bootnodes.is_empty() {
            info!("  Bootnodes: using defaults from swarmspec");
        } else {
            info!("  Bootnodes: {} custom nodes", bootnodes.len());
        }
    } else {
        info!("  Bootnodes: using defaults from swarmspec");
    }

    info!("Availability incentives: {:?}", config.availability.mode);

    info!("Storage configuration:");
    info!("  Capacity: {} chunks", config.storage.capacity_chunks);
    info!(
        "  Redistribution: {}",
        config.storage_incentives.redistribution
    );

    if config.api.grpc {
        info!(
            "gRPC server: {}:{}",
            config.api.grpc_addr, config.api.grpc_port
        );
    }

    if config.api.metrics {
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
        ApiArgs, AvailabilityArgs, DataDirArgs, DatabaseArgs, IdentityArgs, NetworkArgs,
        StorageArgs, StorageIncentiveArgs, SwarmNodeType,
    };

    fn default_node_args() -> NodeArgs {
        NodeArgs {
            node_type: SwarmNodeType::Light,
            datadir: DataDirArgs::default(),
            database: DatabaseArgs::default(),
            network: NetworkArgs::default(),
            availability: AvailabilityArgs::default(),
            storage: StorageArgs::default(),
            storage_incentives: StorageIncentiveArgs::default(),
            api: ApiArgs::default(),
            identity: IdentityArgs::default(),
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
}
