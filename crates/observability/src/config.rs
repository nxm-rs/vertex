//! Configuration structs for observability components.

use std::net::SocketAddr;
use std::path::PathBuf;

use crate::LogFormat;

/// Console/stdout logging configuration.
#[derive(Debug, Clone)]
pub struct StdoutConfig {
    format: LogFormat,
    filter: String,
    ansi: bool,
}

impl StdoutConfig {
    /// Create stdout logging configuration.
    pub fn new(format: LogFormat, filter: impl Into<String>, ansi: bool) -> Self {
        Self {
            format,
            filter: filter.into(),
            ansi,
        }
    }

    pub fn format(&self) -> LogFormat {
        self.format
    }

    pub fn filter(&self) -> &str {
        &self.filter
    }

    pub fn ansi(&self) -> bool {
        self.ansi
    }
}

/// File logging configuration.
#[derive(Debug, Clone)]
pub struct FileConfig {
    directory: PathBuf,
    filename: String,
    format: LogFormat,
    filter: String,
    max_size_mb: u64,
    max_files: usize,
}

impl FileConfig {
    /// Create file logging configuration.
    pub fn new(
        directory: PathBuf,
        filename: impl Into<String>,
        format: LogFormat,
        filter: impl Into<String>,
        max_size_mb: u64,
        max_files: usize,
    ) -> Self {
        Self {
            directory,
            filename: filename.into(),
            format,
            filter: filter.into(),
            max_size_mb,
            max_files,
        }
    }

    pub fn directory(&self) -> &PathBuf {
        &self.directory
    }

    pub fn filename(&self) -> &str {
        &self.filename
    }

    pub fn format(&self) -> LogFormat {
        self.format
    }

    pub fn filter(&self) -> &str {
        &self.filter
    }

    pub fn max_size_mb(&self) -> u64 {
        self.max_size_mb
    }

    pub fn max_files(&self) -> usize {
        self.max_files
    }
}

/// OpenTelemetry OTLP tracing configuration.
#[derive(Debug, Clone)]
pub struct OtlpConfig {
    endpoint: String,
    service_name: String,
    sampling_ratio: f64,
}

impl OtlpConfig {
    /// Create OTLP tracing configuration.
    pub fn new(
        endpoint: impl Into<String>,
        service_name: impl Into<String>,
        sampling_ratio: f64,
    ) -> Self {
        Self {
            endpoint: endpoint.into(),
            service_name: service_name.into(),
            sampling_ratio: sampling_ratio.clamp(0.0, 1.0),
        }
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn service_name(&self) -> &str {
        &self.service_name
    }

    pub fn sampling_ratio(&self) -> f64 {
        self.sampling_ratio
    }
}

/// OTLP log export configuration (e.g., to Loki).
#[derive(Debug, Clone)]
pub struct OtlpLogsConfig {
    endpoint: String,
    service_name: String,
}

impl OtlpLogsConfig {
    /// Create OTLP log export configuration.
    pub fn new(endpoint: impl Into<String>, service_name: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            service_name: service_name.into(),
        }
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn service_name(&self) -> &str {
        &self.service_name
    }
}

/// Prometheus metrics server configuration.
#[derive(Debug, Clone)]
pub struct MetricsServerConfig {
    addr: SocketAddr,
    prefix: String,
    upkeep_interval_secs: u64,
}

impl MetricsServerConfig {
    /// Create metrics server configuration.
    pub fn new(addr: SocketAddr, prefix: impl Into<String>, upkeep_interval_secs: u64) -> Self {
        Self {
            addr,
            prefix: prefix.into(),
            upkeep_interval_secs,
        }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    pub fn upkeep_interval_secs(&self) -> u64 {
        self.upkeep_interval_secs
    }
}
