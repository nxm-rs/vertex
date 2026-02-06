//! Swarm CLI entry point.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use clap::{Parser, Subcommand};
use eyre::Result;
use vertex_node_commands::{HasLogs, InfraArgs, LogArgs, run_cli};
use vertex_node_core::config::FullNodeConfig;
use vertex_node_core::dirs::DataDirs;
use vertex_rpc_server::{GrpcRegistry, RegistersGrpcServices};
use vertex_swarm_builder::{
    BootnodeConfig, ClientConfig, DefaultClientBuilder, DefaultNodeBuilder, DefaultStorerBuilder,
    StorerConfig,
};
use vertex_swarm_node::ProtocolConfig;
use vertex_swarm_node::args::ProtocolArgs;
use vertex_swarm_primitives::SwarmNodeType;
use vertex_swarm_spec::{DefaultSpecParser, Spec, SwarmSpec};

/// Vertex Swarm - Ethereum Swarm Node Implementation
#[derive(Parser)]
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
#[derive(Subcommand)]
pub enum SwarmCommands {
    /// Run a Swarm node.
    Node(SwarmRunNodeArgs),
}

/// Combined arguments for the Swarm 'node' command.
#[derive(clap::Args)]
pub struct SwarmRunNodeArgs {
    /// Swarm network: "mainnet", "testnet", "dev", or path to spec file.
    #[arg(long, default_value = "mainnet", value_parser = DefaultSpecParser::parser())]
    pub swarm: Arc<Spec>,

    /// Generic node infrastructure configuration.
    #[command(flatten)]
    pub infra: InfraArgs,

    /// Swarm protocol configuration.
    #[command(flatten)]
    pub protocol: ProtocolArgs,
}

/// Run the Swarm CLI.
pub async fn run() -> Result<()> {
    run_cli(|cli: SwarmCli| async move {
        let SwarmCommands::Node(args) = cli.command;

        // Spec is already parsed by clap via DefaultSpecParser::parser()
        let spec = args.swarm;

        // Initialize data directories
        let network_name = spec.network_name();
        let dirs = DataDirs::new(network_name, &args.infra.datadir)?;

        // Load and merge configuration
        let config_path = dirs.config_file();
        let mut config = FullNodeConfig::<ProtocolConfig>::load(if config_path.exists() {
            Some(config_path.as_path())
        } else {
            None
        })?;
        config.apply_args(&args.infra, &args.protocol);

        // Build validated configs
        let network = config
            .protocol
            .network_config()
            .map_err(|e| eyre::eyre!("network config error: {}", e))?;
        let identity = config.protocol.identity(spec.clone(), &dirs.network)?;
        let peers_path = dirs.network.join("state").join("peers.json");
        let grpc_addr = socket_addr(&config.infra.api.grpc_addr, config.infra.api.grpc_port);

        // Dispatch based on node type
        match config.protocol.node_type {
            SwarmNodeType::Client => {
                let bandwidth = config
                    .protocol
                    .bandwidth_config()
                    .map_err(|e| eyre::eyre!("bandwidth config error: {}", e))?;

                let node_config =
                    ClientConfig::new(spec, identity, network, bandwidth, peers_path);

                let (task, rpc_providers, _topology) =
                    DefaultClientBuilder::from_config(node_config)
                        .build()
                        .await?
                        .into_parts();
                run_with_grpc(task, &rpc_providers, grpc_addr).await
            }
            SwarmNodeType::Bootnode => {
                let node_config = BootnodeConfig::new(spec, identity, network, peers_path);

                let (task, rpc_providers, _topology) =
                    DefaultNodeBuilder::from_config(node_config)
                        .build()
                        .await?
                        .into_parts();
                run_with_grpc(task, &rpc_providers, grpc_addr).await
            }
            SwarmNodeType::Storer => {
                let bandwidth = config
                    .protocol
                    .bandwidth_config()
                    .map_err(|e| eyre::eyre!("bandwidth config error: {}", e))?;
                let local_store = config.protocol.local_store_config();
                let storage = config.protocol.storage_config();

                let node_config = StorerConfig::new(
                    spec,
                    identity,
                    network,
                    bandwidth,
                    local_store,
                    storage,
                    peers_path,
                );

                let (task, rpc_providers, _topology) =
                    DefaultStorerBuilder::from_config(node_config)
                        .build()
                        .await?
                        .into_parts();
                run_with_grpc(task, &rpc_providers, grpc_addr).await
            }
        }
    })
    .await
}

/// Run node task with gRPC server.
async fn run_with_grpc<P: RegistersGrpcServices>(
    task: vertex_swarm_api::NodeTask,
    providers: &P,
    grpc_addr: SocketAddr,
) -> Result<()> {
    // Build gRPC server
    let mut registry = GrpcRegistry::new();
    providers.register_grpc_services(&mut registry);
    let server = registry.into_server(grpc_addr)?;
    tracing::info!(%grpc_addr, "Starting gRPC server");

    // Run until shutdown
    tokio::select! {
        result = server.serve_with_shutdown(async {
            tokio::signal::ctrl_c().await.ok();
            tracing::info!("Received shutdown signal");
        }) => {
            result?;
        }
        _ = task => {
            tracing::info!("Node task completed");
        }
    }
    Ok(())
}

fn socket_addr(addr: &str, port: u16) -> SocketAddr {
    SocketAddr::new(
        addr.parse().unwrap_or(IpAddr::from([127, 0, 0, 1])),
        port,
    )
}
