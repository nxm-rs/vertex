//! Swarm CLI entry point.

use clap::{Parser, Subcommand};
use eyre::Result;
use vertex_node_builder::NodeBuilder;
use vertex_node_commands::{HasLogs, HasTracing, InfraArgs, LogArgs, TracingArgs, run_cli};
use vertex_node_core::config::FullNodeConfig;
use vertex_node_core::dirs::DataDirs;
use vertex_node_core::version;
#[cfg(feature = "storer")]
use vertex_swarm_builder::StorerConfig;
use vertex_swarm_builder::{BootnodeConfig, ClientConfig};
use vertex_swarm_node::ProtocolConfig;
use vertex_swarm_node::args::ProtocolArgs;
use vertex_swarm_primitives::SwarmNodeType;
use vertex_swarm_spec::SwarmSpec;
use vertex_tasks::TaskExecutor;

/// Vertex Swarm - Ethereum Swarm Node Implementation
#[derive(Parser)]
#[command(author, version = version::LONG_VERSION.as_str(), about, long_about = None)]
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

/// Whether the shared database persists by default for this node type.
///
/// Storers persist so a restart re-converges from the known address book;
/// clients and bootnodes stay ephemeral.
fn db_persist_default(node_type: SwarmNodeType) -> bool {
    matches!(node_type, SwarmNodeType::Storer)
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
        config.protocol.override_node_type(node_type);

        // Resolve database config from CLI args. Storers default to persistent
        // so a restart seeds re-convergence from the known address book; clients
        // and bootnodes stay in-memory. --db.in-memory opts out.
        let database_config = config.infra.database.database_config(
            dirs.network.join("db").join("vertex.redb"),
            db_persist_default(node_type),
        );

        // Build metrics config from CLI args
        let metrics_config = args.infra.observability.metrics.metrics_config();

        // Aggregated histogram bucket configs from the launch path
        let histogram_buckets = vertex_observability::HistogramRegistry::new()
            .register_all(&vertex_swarm_builder::histogram_buckets())
            .build();

        // Build the launch context once and install metrics before any subsystem
        // records, then flow the same context through the protocol shell.
        let executor = TaskExecutor::current();
        let builder = NodeBuilder::new()
            .with_launch_context(executor, dirs.clone())
            .with_database_config(database_config)
            .with_metrics(metrics_config, &histogram_buckets)?
            .start_metrics_server()
            .await?;

        // Build validated configs. Announce the build-stamped agent string
        // (`vertex/<version>-<sha>`) over libp2p identify; the binary is the
        // injection point because the lower node and builder crates stay free of
        // the version crate.
        let network = config
            .protocol
            .network_config()
            .map_err(|e| eyre::eyre!("network config error: {}", e))?
            .with_agent_version(version::AGENT_VERSION.clone());
        let identity = config.protocol.identity(spec.clone(), &dirs.network)?;

        // Dispatch based on node type. Every node type flows through the same
        // shell: build the validated config, then `with_protocol().launch()`.
        match node_type {
            SwarmNodeType::Client => {
                let bandwidth = config.protocol.bandwidth_config();
                let local_store = config.protocol.local_store_config();
                let chain = config.protocol.chain_config();
                let swap = config.protocol.swap_config();

                let node_config =
                    ClientConfig::new(spec, identity, network, bandwidth, local_store, chain, swap);

                let handle = builder.with_protocol(node_config).launch().await?;
                if config.infra.api.grpc {
                    handle.serve_grpc(config.infra.api.grpc_socket_addr())?;
                }
                handle.wait_for_shutdown().await;
                Ok(())
            }
            SwarmNodeType::Bootnode => {
                let node_config = BootnodeConfig::new(spec, identity, network);

                let handle = builder.with_protocol(node_config).launch().await?;
                if config.infra.api.grpc {
                    handle.serve_grpc(config.infra.api.grpc_socket_addr())?;
                }
                handle.wait_for_shutdown().await;
                Ok(())
            }
            #[cfg(feature = "storer")]
            SwarmNodeType::Storer => {
                let bandwidth = config.protocol.bandwidth_config();
                let local_store = config.protocol.local_store_config();
                let storage = config.protocol.storage_config();
                let chain = config.protocol.chain_config();
                let swap = config.protocol.swap_config();

                let node_config = StorerConfig::new(
                    spec,
                    identity,
                    network,
                    bandwidth,
                    local_store,
                    storage,
                    chain,
                    swap,
                );

                let handle = builder.with_protocol(node_config).launch().await?;
                if config.infra.api.grpc {
                    handle.serve_grpc(config.infra.api.grpc_socket_addr())?;
                }
                handle.wait_for_shutdown().await;
                Ok(())
            }
            // The default binary compiles without the storer cone; refuse the
            // role at runtime rather than panicking.
            #[cfg(not(feature = "storer"))]
            SwarmNodeType::Storer => Err(eyre::eyre!(
                "this build was compiled without storer support; rebuild with `--features storer`"
            )),
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_persist_default_is_storer_only() {
        assert!(db_persist_default(SwarmNodeType::Storer));
        assert!(!db_persist_default(SwarmNodeType::Client));
        assert!(!db_persist_default(SwarmNodeType::Bootnode));
    }
}
