//! Swarm CLI entry point.

use clap::{Parser, Subcommand};
use eyre::Result;
use vertex_node_builder::LaunchContextExt;
use vertex_node_commands::{HasLogs, HasTracing, InfraArgs, LogArgs, TracingArgs, run_cli};
use vertex_node_core::config::FullNodeConfig;
use vertex_node_core::dirs::DataDirs;
use vertex_swarm_builder::SwarmBuildConfig;
use vertex_swarm_node::ProtocolConfig;
use vertex_swarm_node::args::ProtocolArgs;
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
            .register_all(vertex_swarm_net_pingpong::metrics::HISTOGRAM_BUCKETS)
            .register_all(vertex_storage_redb::metrics::HISTOGRAM_BUCKETS)
            .build();

        // Initialize metrics via launch context
        let executor = TaskExecutor::current();
        let launch_ctx = (executor.clone(), dirs.clone())
            .with_metrics(metrics_config, &histogram_buckets)?
            .start_metrics_server()
            .await?;

        // Build config holds raw inputs; validation happens progressively
        // inside the builder chain when the protocol is launched.
        let node_config = SwarmBuildConfig::new(config.protocol, spec, dirs.network.clone());
        let grpc_addr = config.infra.api.grpc_socket_addr();

        launch_ctx
            .into_node_builder()
            .with_protocol(node_config)
            .launch(grpc_addr)
            .await?
            .wait_for_shutdown()
            .await;

        Ok(())
    })
    .await
}
