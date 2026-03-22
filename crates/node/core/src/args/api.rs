//! API server CLI arguments.

use std::net::{IpAddr, SocketAddr};

use crate::constants::{DEFAULT_GRPC_PORT, DEFAULT_LOCALHOST_ADDR};
use clap::Args;
use serde::{Deserialize, Serialize};

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

impl ApiArgs {
    /// Compute the gRPC socket address from configured host and port.
    pub fn grpc_socket_addr(&self) -> SocketAddr {
        let ip: IpAddr = self.grpc_addr.parse().unwrap_or(IpAddr::from([127, 0, 0, 1]));
        SocketAddr::new(ip, self.grpc_port)
    }
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
