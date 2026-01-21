//! Hive protocol for peer exchange.
//!
//! The hive protocol allows nodes to discover peers by exchanging information
//! about known peers. When connecting to a peer, nodes exchange lists of peers
//! they know, helping the network self-organize.
//!
//! # Protocol Flow
//!
//! 1. Node A connects to Node B
//! 2. A sends a `Peers` message with peers it knows (filtered by relevance to B)
//! 3. B responds with peers it knows (filtered by relevance to A)
//! 4. Both nodes add relevant peers to their routing tables
//!
//! # Peer Selection
//!
//! When sharing peers, nodes filter based on:
//! - Proximity to the recipient (share peers that are useful to them)
//! - Freshness (prefer recently-seen peers)
//! - Avoid sharing peers the recipient already knows

// TODO: Implement hive protocol codec and behaviour
// This will use libp2p's request-response pattern

/// Maximum number of peers to share in a single hive exchange.
pub const MAX_PEERS_PER_EXCHANGE: usize = 30;

/// Configuration for the hive protocol.
#[derive(Debug, Clone)]
pub struct HiveConfig {
    /// Maximum peers to share per exchange.
    pub max_peers: usize,

    /// Whether to actively request peers from connections.
    pub active_discovery: bool,
}

impl Default for HiveConfig {
    fn default() -> Self {
        Self {
            max_peers: MAX_PEERS_PER_EXCHANGE,
            active_discovery: true,
        }
    }
}

// Placeholder for future implementation
// The actual protocol will be implemented using libp2p request-response
// with protobuf messages similar to the handshake protocol
