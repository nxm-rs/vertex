//! Crate-internal constants.

/// Default P2P listen port.
pub(crate) const DEFAULT_P2P_PORT: u16 = 1634;

/// Default listen address.
pub(crate) const DEFAULT_LISTEN_ADDR: &str = "0.0.0.0";

/// Default maximum peers.
pub(crate) const DEFAULT_MAX_PEERS: usize = 50;

/// Default idle timeout in seconds.
pub(crate) const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 60;
