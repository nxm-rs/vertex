//! Constants for node infrastructure configuration defaults.

/// Default port for the gRPC server.
pub(crate) const DEFAULT_GRPC_PORT: u16 = 1635;

/// Default port for metrics.
pub(crate) const DEFAULT_METRICS_PORT: u16 = 1637;

/// Default bind address for local-only services (gRPC, metrics).
pub(crate) const DEFAULT_LOCALHOST_ADDR: &str = "127.0.0.1";

/// Default data directory name.
pub(crate) const DEFAULT_DATA_DIR_NAME: &str = "vertex";
