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

impl Default for ApiArgs {
    fn default() -> Self {
        Self {
            grpc: false,
            grpc_addr: DEFAULT_LOCALHOST_ADDR.to_string(),
            grpc_port: DEFAULT_GRPC_PORT,
        }
    }
}

impl ApiArgs {
    /// gRPC socket address; falls back to localhost if the configured address is
    /// unparseable.
    pub fn grpc_socket_addr(&self) -> SocketAddr {
        let ip: IpAddr = self.grpc_addr.parse().unwrap_or_else(|_| {
            tracing::warn!(
                addr = %self.grpc_addr,
                "Invalid gRPC address, falling back to localhost"
            );
            [127, 0, 0, 1].into()
        });
        SocketAddr::new(ip, self.grpc_port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grpc_socket_addr_parses_valid_address() {
        let args = ApiArgs {
            grpc: true,
            grpc_addr: "0.0.0.0".to_string(),
            grpc_port: 1700,
        };
        assert_eq!(
            args.grpc_socket_addr(),
            SocketAddr::new(IpAddr::from([0, 0, 0, 0]), 1700)
        );
    }

    #[test]
    fn grpc_socket_addr_falls_back_to_localhost() {
        let args = ApiArgs {
            grpc: true,
            grpc_addr: "not-an-address".to_string(),
            grpc_port: 1701,
        };
        assert_eq!(
            args.grpc_socket_addr(),
            SocketAddr::new(IpAddr::from([127, 0, 0, 1]), 1701)
        );
    }
}
