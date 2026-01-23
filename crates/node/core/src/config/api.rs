//! API configuration for TOML persistence.

use crate::constants::*;
use serde::{Deserialize, Serialize};

/// API configuration (TOML-serializable).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiConfig {
    /// Whether to enable gRPC server
    #[serde(default)]
    pub grpc_enabled: bool,

    /// gRPC server address
    #[serde(default = "default_grpc_addr")]
    pub grpc_addr: String,

    /// gRPC server port
    #[serde(default = "default_grpc_port")]
    pub grpc_port: u16,

    /// Whether to enable metrics HTTP server
    #[serde(default)]
    pub metrics_enabled: bool,

    /// Metrics server address
    #[serde(default = "default_metrics_addr")]
    pub metrics_addr: String,

    /// Metrics server port
    #[serde(default = "default_metrics_port")]
    pub metrics_port: u16,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            grpc_enabled: false,
            grpc_addr: default_grpc_addr(),
            grpc_port: default_grpc_port(),
            metrics_enabled: false,
            metrics_addr: default_metrics_addr(),
            metrics_port: default_metrics_port(),
        }
    }
}

fn default_grpc_addr() -> String {
    DEFAULT_LOCALHOST_ADDR.to_string()
}

fn default_grpc_port() -> u16 {
    DEFAULT_GRPC_PORT
}

fn default_metrics_addr() -> String {
    DEFAULT_LOCALHOST_ADDR.to_string()
}

fn default_metrics_port() -> u16 {
    DEFAULT_METRICS_PORT
}
