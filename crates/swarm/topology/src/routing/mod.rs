//! Kademlia-based peer routing for Swarm overlay network.

mod config;
mod kademlia;
mod pslice;

pub use config::{
    KademliaConfig, DEFAULT_CLIENT_RESERVED_SLOTS, DEFAULT_HIGH_WATERMARK, DEFAULT_LOW_WATERMARK,
    DEFAULT_MANAGE_INTERVAL, DEFAULT_MAX_BALANCED_CANDIDATES, DEFAULT_MAX_CONNECT_ATTEMPTS,
    DEFAULT_MAX_NEIGHBOR_ATTEMPTS, DEFAULT_MAX_NEIGHBOR_CANDIDATES, DEFAULT_SATURATION_PEERS,
};
pub use kademlia::{KademliaRouting, PeerFailureProvider, RoutingStats};
pub use pslice::{PSlice, MAX_PO};
