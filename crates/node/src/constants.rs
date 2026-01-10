//! Constants used throughout the Vertex Swarm node.

/// Default port for the Swarm network's TCP connections
pub const DEFAULT_P2P_PORT: u16 = 1634;

/// Default port for the Swarm network's UDP discovery
pub const DEFAULT_DISCOVERY_PORT: u16 = 1634;

/// Default port for the HTTP API
pub const DEFAULT_HTTP_API_PORT: u16 = 1635;

/// Default port for the gRPC API
pub const DEFAULT_GRPC_API_PORT: u16 = 1636;

/// Default port for metrics
pub const DEFAULT_METRICS_PORT: u16 = 1637;

/// Default maximum number of peers
pub const DEFAULT_MAX_PEERS: usize = 50;

/// Default maximum number of concurrent requests
pub const DEFAULT_MAX_CONCURRENT_REQUESTS: usize = 100;

/// Default buffer size for chunk data (4KB)
pub const DEFAULT_CHUNK_SIZE: usize = 4 * 1024;

/// Default data directory name
pub const DEFAULT_DATA_DIR_NAME: &str = "vertex";

/// Default pruning parameters

/// Default maximum number of chunks to store
pub const DEFAULT_MAX_CHUNKS: usize = 1_000_000;

/// Default target number of chunks
pub const DEFAULT_TARGET_CHUNKS: usize = 500_000;

/// Default minimum number of chunks
pub const DEFAULT_MIN_CHUNKS: usize = 100_000;

/// Default reserve storage percentage
pub const DEFAULT_RESERVE_PERCENTAGE: u8 = 10;

/// Default daily bandwidth allowance (free tier) in bytes (1MB)
pub const DEFAULT_DAILY_BANDWIDTH_ALLOWANCE: u64 = 1_000_000;

/// Default payment threshold in bytes (10MB)
pub const DEFAULT_PAYMENT_THRESHOLD: u64 = 10_000_000;

/// Default payment tolerance in bytes (5MB)
pub const DEFAULT_PAYMENT_TOLERANCE: u64 = 5_000_000;

/// Default disconnect threshold in bytes (50MB)
pub const DEFAULT_DISCONNECT_THRESHOLD: u64 = 50_000_000;

/// Default maximum storage size (in GB)
pub const DEFAULT_MAX_STORAGE_SIZE_GB: u64 = 10;

/// Convert GB to bytes
pub const GB_TO_BYTES: u64 = 1024 * 1024 * 1024;
