//! Swarm CLI entry point.

use std::future::Future;
use std::sync::Arc;

use alloy_primitives::B256;
use clap::{Parser, Subcommand};
use eyre::{Result, WrapErr};
use vertex_node_builder::LaunchContext;
use vertex_node_commands::{HasLogs, InfraArgs, LogArgs, run_cli};
use vertex_node_core::config::FullNodeConfig;
use vertex_node_core::dirs::DataDirs;
use vertex_swarm_builder::{
    SwarmLaunchContext, create_and_save_signer, load_signer_from_keystore, resolve_password,
};
use vertex_swarm_identity::Identity;
use vertex_swarm_node::SwarmConfig;
use vertex_swarm_node::args::ProtocolArgs;
use vertex_swarm_peermanager::{FilePeerStore, PeerStore};
use vertex_swarmspec::{Hive, SwarmSpec, init_mainnet, init_testnet};
use vertex_tasks::TaskExecutor;

/// Vertex Swarm - Ethereum Swarm Node Implementation
#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
pub struct SwarmCli {
    /// Logging configuration (applies to all subcommands).
    #[command(flatten)]
    pub logs: LogArgs,

    /// Subcommand to execute.
    #[command(subcommand)]
    pub command: SwarmCommands,
}

impl HasLogs for SwarmCli {
    fn logs(&self) -> &LogArgs {
        &self.logs
    }
}

/// Available Swarm commands.
#[derive(Debug, Subcommand)]
pub enum SwarmCommands {
    /// Run a Swarm node.
    Node(SwarmRunNodeArgs),
}

/// Combined arguments for the Swarm 'node' command.
///
/// This composes generic infrastructure args with Swarm protocol-specific args.
#[derive(Debug, clap::Args)]
pub struct SwarmRunNodeArgs {
    /// Generic node infrastructure configuration.
    #[command(flatten)]
    pub infra: InfraArgs,

    /// Swarm protocol configuration.
    #[command(flatten)]
    pub swarm: ProtocolArgs,
}

/// Run the Swarm node CLI with a user-provided closure.
pub async fn run<F, Fut>(runner: F) -> Result<()>
where
    F: FnOnce(SwarmLaunchContext, SwarmRunNodeArgs) -> Fut,
    Fut: Future<Output = Result<()>>,
{
    run_cli(|cli: SwarmCli| async move {
        // Extract node args from CLI
        let SwarmCommands::Node(args) = cli.command;

        // Build Swarm launch context
        let ctx = build_launch_context(&args).await?;

        // Call user's runner
        runner(ctx, args).await
    })
    .await
}

/// Build a Swarm launch context from CLI arguments.
async fn build_launch_context(args: &SwarmRunNodeArgs) -> Result<SwarmLaunchContext> {
    use tracing::debug;

    // Validate argument combinations
    args.swarm.validate().map_err(|e| eyre::eyre!(e))?;

    // Get task executor
    let executor = TaskExecutor::current();

    // Determine which network to connect to
    let spec = resolve_network_spec(&args.swarm)?;

    // Initialize data directories
    let dirs = DataDirs::new(spec.network_name(), &args.infra.datadir)?;

    // Load configuration
    let config_path = dirs.config_file();
    let mut config = FullNodeConfig::<SwarmConfig>::load(if config_path.exists() {
        Some(config_path.as_path())
    } else {
        None
    })?;

    // Apply CLI overrides
    config.apply_args(&args.infra, &args.swarm);

    let node_type = config.protocol.node_type;

    // Create peer store
    let peers_file = dirs.network.join("state").join("peers.json");
    let peer_store: Arc<dyn PeerStore> = Arc::new(
        FilePeerStore::new_with_create_dir(&peers_file)
            .wrap_err_with(|| format!("failed to open peers database: {}", peers_file.display()))?,
    );

    // Determine if we need ephemeral or persistent identity
    let use_ephemeral =
        config.protocol.identity.ephemeral || !node_type.requires_persistent_identity();

    // Create identity
    let identity = if use_ephemeral {
        Identity::random(spec.clone(), node_type)
    } else {
        // Load or create persistent identity
        let keystore_path = dirs.network.join("keystore").join("swarm");

        let password = resolve_password(
            config.protocol.identity.password.as_deref(),
            config
                .protocol
                .identity
                .password_file
                .as_ref()
                .map(|p| p.to_string_lossy())
                .as_deref(),
        )?;

        let signer = if keystore_path.exists() {
            load_signer_from_keystore(&keystore_path, &password)?
        } else {
            create_and_save_signer(&keystore_path, &password)?
        };

        let nonce = config.protocol.identity.nonce.unwrap_or_else(|| {
            use rand::Rng;
            let mut bytes = [0u8; 32];
            rand::rng().fill(&mut bytes);
            debug!("Generated new nonce");
            B256::from(bytes)
        });

        Identity::new(signer, nonce, spec.clone(), node_type)
    };

    let base = LaunchContext::new(executor, dirs, ());

    Ok(SwarmLaunchContext::new(
        base, config, spec, identity, peer_store, peers_file,
    ))
}

/// Resolve the network specification from CLI arguments.
fn resolve_network_spec(args: &ProtocolArgs) -> Result<Arc<Hive>> {
    if args.is_mainnet() {
        Ok(init_mainnet())
    } else if args.is_testnet() {
        Ok(init_testnet())
    } else {
        // Default to mainnet
        Ok(init_mainnet())
    }
}
