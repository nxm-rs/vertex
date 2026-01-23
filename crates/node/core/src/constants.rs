//! Constants used throughout the Vertex Swarm node.
//!
//! All magic numbers and default values should be defined here or at the top
//! of specific modules if they are tightly coupled to that module's logic.

// =============================================================================
// Network Ports
// =============================================================================

/// Default port for the Swarm network's TCP connections.
pub const DEFAULT_P2P_PORT: u16 = 1634;

/// Default port for the Swarm network's UDP discovery.
pub const DEFAULT_DISCOVERY_PORT: u16 = 1634;

/// Default port for the gRPC server.
pub const DEFAULT_GRPC_PORT: u16 = 1635;

/// Default port for metrics.
pub const DEFAULT_METRICS_PORT: u16 = 1637;

// =============================================================================
// Network Addresses
// =============================================================================

/// Default listen address for P2P connections (all interfaces).
pub const DEFAULT_LISTEN_ADDR: &str = "0.0.0.0";

/// Default bind address for local-only services (gRPC, metrics).
pub const DEFAULT_LOCALHOST_ADDR: &str = "127.0.0.1";

// =============================================================================
// Network Peer Limits
// =============================================================================

/// Default maximum number of peers.
pub const DEFAULT_MAX_PEERS: usize = 50;

/// Default maximum number of concurrent requests.
pub const DEFAULT_MAX_CONCURRENT_REQUESTS: usize = 100;

// =============================================================================
// Network Timeouts & Intervals
// =============================================================================

/// Default connection idle timeout in seconds.
pub const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 30;

/// Default ping interval in seconds.
pub const DEFAULT_PING_INTERVAL_SECS: u64 = 15;

// =============================================================================
// Protocol Identification
// =============================================================================

/// Protocol version string for identify protocol.
pub const PROTOCOL_VERSION: &str = "/vertex/1.0.0";

/// Default NAT traversal method.
pub const DEFAULT_NAT_METHOD: &str = "upnp";

// =============================================================================
// Storage Configuration
// =============================================================================

/// Default data directory name.
pub const DEFAULT_DATA_DIR_NAME: &str = "vertex";

// =============================================================================
// Cryptographic Constants
// =============================================================================

/// Size of a nonce in bytes.
pub const NONCE_SIZE_BYTES: usize = 32;

// =============================================================================
// File System
// =============================================================================

/// Restrictive file permissions for sensitive files (Unix: owner read/write only).
#[cfg(unix)]
pub const SENSITIVE_FILE_MODE: u32 = 0o600;
