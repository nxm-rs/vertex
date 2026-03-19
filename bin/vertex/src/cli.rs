//! Swarm CLI entry point.

use std::net::{IpAddr, SocketAddr};

use clap::{Parser, Subcommand};
use eyre::Result;
use vertex_node_builder::LaunchContextExt;
use vertex_node_commands::{HasLogs, HasTracing, InfraArgs, LogArgs, TracingArgs, run_cli};
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
use vertex_swarm_spec::SwarmSpec;
use vertex_tasks::TaskExecutor;

/// Vertex Swarm - Ethereum Swarm Node Implementation
#[derive(Parser)]
#[command(author, version, about, long_about = None)]
pub struct SwarmCli {
    /// Logging configuration (applies to all subcommands).
    #[command(flatten)]
    pub logs: LogArgs,

    /// OpenTelemetry tracing configuration (applies to all subcommands).
    #[command(flatten)]
    pub tracing: TracingArgs,

    /// Subcommand to execute.
    #[command(subcommand)]
    pub command: SwarmCommands,
}

impl HasLogs for SwarmCli {
    fn logs(&self) -> &LogArgs {
        &self.logs
    }
}

impl HasTracing for SwarmCli {
    fn tracing(&self) -> &TracingArgs {
        &self.tracing
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

        // Spec and node type from ProtocolArgs
        let spec = args.protocol.spec.swarm.clone();
        let node_type = args.protocol.spec.node_type();

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
        config.protocol.set_node_type(node_type);

        // Build metrics config from CLI args
        let metrics_config = args.infra.observability.metrics.metrics_config();

        // Collect histogram bucket configs from protocol crates
        let histogram_buckets = vertex_observability::HistogramRegistry::new()
            .register_all(vertex_swarm_net_headers::metrics::HISTOGRAM_BUCKETS)
            .register_all(vertex_swarm_topology::metrics::HISTOGRAM_BUCKETS)
            .register_all(vertex_swarm_net_handshake::metrics::HISTOGRAM_BUCKETS)
            .register_all(vertex_swarm_net_hive::metrics::HISTOGRAM_BUCKETS)
            .register_all(vertex_swarm_net_identify::metrics::HISTOGRAM_BUCKETS)
            .register_all(vertex_storage_redb::metrics::HISTOGRAM_BUCKETS)
            .build();

        // Initialize metrics via launch context
        let executor = TaskExecutor::current();
        let launch_ctx = (executor.clone(), dirs.clone())
            .with_metrics(metrics_config, &histogram_buckets)?
            .start_metrics_server()
            .await?;

        // Build validated configs
        let network = config
            .protocol
            .network_config()
            .map_err(|e| eyre::eyre!("network config error: {}", e))?;
        let identity = config.protocol.identity(spec.clone(), &dirs.network)?;
        let grpc_addr = socket_addr(&config.infra.api.grpc_addr, config.infra.api.grpc_port);

        // Dispatch based on node type
        match node_type {
            SwarmNodeType::Client => {
                let bandwidth = config
                    .protocol
                    .bandwidth_config()
                    .map_err(|e| eyre::eyre!("bandwidth config error: {}", e))?;

                let node_config = ClientConfig::new(spec, identity, network, bandwidth);

                let (task_fn, rpc_providers) = DefaultClientBuilder::from_config(node_config)
                    .build(&launch_ctx)
                    .await?
                    .into_parts();
                run_with_grpc(task_fn, rpc_providers, grpc_addr).await
            }
            SwarmNodeType::Bootnode => {
                let node_config = BootnodeConfig::new(spec, identity, network);

                let (task_fn, rpc_providers) = DefaultNodeBuilder::from_config(node_config)
                    .build(&launch_ctx)
                    .await?
                    .into_parts();
                run_with_grpc(task_fn, rpc_providers, grpc_addr).await
            }
            SwarmNodeType::Storer => {
                let bandwidth = config
                    .protocol
                    .bandwidth_config()
                    .map_err(|e| eyre::eyre!("bandwidth config error: {}", e))?;
                let local_store = config.protocol.local_store_config();
                let storage = config.protocol.storage_config();

                let node_config =
                    StorerConfig::new(spec, identity, network, bandwidth, local_store, storage);

                let (task_fn, rpc_providers) = DefaultStorerBuilder::from_config(node_config)
                    .build(&launch_ctx)
                    .await?
                    .into_parts();
                run_with_grpc(task_fn, rpc_providers, grpc_addr).await
            }
        }
    })
    .await
}

/// Run node task with gRPC server.
///
/// Uses the executor's shutdown signal for graceful shutdown coordination.
/// The caller (run_cli) handles Ctrl+C and fires the shutdown signal.
async fn run_with_grpc<P: RegistersGrpcServices + Send + Sync + 'static>(
    task_fn: vertex_tasks::NodeTaskFn,
    providers: P,
    grpc_addr: SocketAddr,
) -> Result<()> {
    // Build gRPC server
    let mut registry = GrpcRegistry::new();
    providers.register_grpc_services(&mut registry);
    let server = registry.into_server(grpc_addr)?;
    tracing::info!(%grpc_addr, "Starting gRPC server");

    // Get the executor's shutdown signal for the gRPC server.
    // This signal is fired by run_cli when Ctrl+C is received.
    let executor = TaskExecutor::current();
    let grpc_shutdown = executor.on_shutdown_signal().clone();

    // Spawn the node task with graceful shutdown support.
    // This passes a GracefulShutdown signal to the task function.
    let node_handle = executor.spawn_critical_with_graceful_shutdown_signal("swarm.node", task_fn);

    // Run until shutdown signal fires or node task completes
    tokio::select! {
        result = server.serve_with_shutdown(grpc_shutdown) => {
            result?;
        }
        result = node_handle => {
            match result {
                Ok(()) => tracing::info!("Node task completed"),
                Err(e) => tracing::error!(error = %e, "Node task panicked"),
            }
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
