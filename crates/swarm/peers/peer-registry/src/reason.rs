//! Dial reason for Swarm connections.

/// Reason for initiating a dial.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DialReason {
    /// Peer discovered via Hive protocol.
    Discovery,
    /// Connecting to a bootnode.
    Bootnode,
    /// Connecting to a trusted peer.
    Trusted,
    /// User-initiated dial command.
    Command,
}

impl DialReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            DialReason::Discovery => "discovery",
            DialReason::Bootnode => "bootnode",
            DialReason::Trusted => "trusted",
            DialReason::Command => "command",
        }
    }
}

impl std::fmt::Display for DialReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}
