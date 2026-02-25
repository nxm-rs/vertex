//! Dial reason for Swarm connections.

/// Reason for initiating a dial.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, strum::Display, strum::IntoStaticStr)]
#[strum(serialize_all = "lowercase")]
pub enum DialReason {
    /// Peer discovered via Hive protocol (already verified or from peer store).
    Discovery,
    /// Connecting to a bootnode.
    Bootnode,
    /// Connecting to a trusted peer.
    Trusted,
    /// User-initiated dial command.
    Command,
}
