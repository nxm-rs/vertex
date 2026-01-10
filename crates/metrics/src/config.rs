//! Configuration for metrics and observability

use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

/// Main metrics configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsConfig {
    /// Prometheus metrics configuration
    #[serde(default)]
    pub prometheus: PrometheusConfig,

    /// Logging configuration
    #[serde(default)]
    pub logging: LoggingConfig,

    /// Tracing configuration
    #[serde(default)]
    pub tracing: TracingConfig,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            prometheus: PrometheusConfig::default(),
            logging: LoggingConfig::default(),
            tracing: TracingConfig::default(),
        }
    }
}

/// Configuration for prometheus metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrometheusConfig {
    /// Whether prometheus metrics are enabled
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// HTTP endpoint for prometheus metrics
    pub endpoint: Option<SocketAddr>,

    /// Prefix for all metrics
    #[serde(default = "default_prefix")]
    pub prefix: String,

    /// How often to run recorder upkeep in seconds
    #[serde(default = "default_upkeep_interval")]
    pub upkeep_interval_secs: u64,
}

impl Default for PrometheusConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            endpoint: None,
            prefix: default_prefix(),
            upkeep_interval_secs: default_upkeep_interval(),
        }
    }
}

/// Configuration for logging
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    /// Whether logging is enabled
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Minimum log level to record
    #[serde(default)]
    pub level: LogLevel,

    /// Whether to use JSON formatting for logs
    #[serde(default)]
    pub json: bool,

    /// Directory for log files
    pub log_dir: Option<String>,

    /// Maximum size of log files in MB
    #[serde(default = "default_log_file_size")]
    pub max_file_size_mb: u64,

    /// Maximum number of log files to keep
    #[serde(default = "default_log_file_count")]
    pub max_files: usize,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            level: LogLevel::default(),
            json: false,
            log_dir: None,
            max_file_size_mb: default_log_file_size(),
            max_files: default_log_file_count(),
        }
    }
}

/// Configuration for tracing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TracingConfig {
    /// Whether tracing is enabled
    #[serde(default)]
    pub enabled: bool,

    /// Tracing system to use
    #[serde(default)]
    pub system: TracingSystem,

    /// Jaeger agent endpoint
    pub jaeger_endpoint: Option<String>,

    /// OTLP endpoint
    pub otlp_endpoint: Option<String>,

    /// Service name for tracing
    #[serde(default = "default_service_name")]
    pub service_name: String,
}

impl Default for TracingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            system: TracingSystem::default(),
            jaeger_endpoint: None,
            otlp_endpoint: None,
            service_name: default_service_name(),
        }
    }
}

/// Log level
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    /// Error level
    Error,
    /// Warning level
    Warn,
    /// Info level
    Info,
    /// Debug level
    Debug,
    /// Trace level
    Trace,
}

impl Default for LogLevel {
    fn default() -> Self {
        Self::Info
    }
}

impl From<LogLevel> for tracing::Level {
    fn from(level: LogLevel) -> Self {
        match level {
            LogLevel::Error => tracing::Level::ERROR,
            LogLevel::Warn => tracing::Level::WARN,
            LogLevel::Info => tracing::Level::INFO,
            LogLevel::Debug => tracing::Level::DEBUG,
            LogLevel::Trace => tracing::Level::TRACE,
        }
    }
}

/// Tracing system to use
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TracingSystem {
    /// Jaeger tracing
    Jaeger,
    /// OTLP tracing
    Otlp,
}

impl Default for TracingSystem {
    fn default() -> Self {
        Self::Jaeger
    }
}

// Default values for configuration

fn default_true() -> bool {
    true
}

fn default_prefix() -> String {
    "vertex".to_string()
}

fn default_upkeep_interval() -> u64 {
    5
}

fn default_log_file_size() -> u64 {
    100
}

fn default_log_file_count() -> usize {
    5
}

fn default_service_name() -> String {
    "vertex-swarm".to_string()
}
