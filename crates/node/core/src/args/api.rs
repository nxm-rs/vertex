//! API server CLI arguments.

use crate::constants::{DEFAULT_GRPC_PORT, DEFAULT_LOCALHOST_ADDR};
use clap::Args;
use serde::{Deserialize, Serialize};
use vertex_node_api::NodeRpcConfig;

/// API server configuration.
#[derive(Debug, Args, Clone, Serialize, Deserialize)]
#[command(next_help_heading = "API")]
#[serde(default)]
pub struct ApiArgs {
    /// Enable the gRPC server.
    #[arg(long = "grpc")]
    pub grpc: bool,

    /// gRPC server listen address.
    #[arg(long = "grpc.addr", default_value = DEFAULT_LOCALHOST_ADDR)]
    pub grpc_addr: String,

    /// gRPC server listen port.
    #[arg(long = "grpc.port", default_value_t = DEFAULT_GRPC_PORT)]
    pub grpc_port: u16,
}

impl Default for ApiArgs {
    fn default() -> Self {
        Self {
            grpc: false,
            grpc_addr: DEFAULT_LOCALHOST_ADDR.to_string(),
            grpc_port: DEFAULT_GRPC_PORT,
        }
    }
}

impl NodeRpcConfig for ApiArgs {
    fn grpc_enabled(&self) -> bool {
        self.grpc
    }

    fn grpc_addr(&self) -> &str {
        &self.grpc_addr
    }

    fn grpc_port(&self) -> u16 {
        self.grpc_port
    }
}
