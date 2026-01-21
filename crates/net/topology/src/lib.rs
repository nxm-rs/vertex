//! Swarm network topology management.
//!
//! This crate handles peer discovery, routing table management, and network topology
//! for Swarm nodes. It can operate standalone (bootnode mode) or as part of a full node.
//!
//! # Architecture
//!
//! The topology system has several components:
//!
//! - **Kademlia**: The DHT-based routing table that organizes peers by their proximity
//!   (XOR distance) to the local node's overlay address. Determines storage responsibility
//!   and routing decisions.
//!
//! - **Hive**: The peer exchange protocol. Nodes share information about known peers,
//!   allowing the network to self-organize and maintain connectivity.
//!
//! - **Bootnode**: Initial network entry. Manages connection to bootstrap nodes.
//!   DNS resolution of `/dnsaddr/` multiaddrs is handled automatically by libp2p's
//!   DNS transport when dialing.
//!
//! - **Manager**: Peer lifecycle management. Handles connection state, disconnection,
//!   and maintains the desired number of peers per bin.
//!
//! # Modes
//!
//! The topology system supports different operational modes:
//!
//! - **Full node**: Participates fully in topology, stores chunks, serves requests
//! - **Light node**: Connects to peers but doesn't store chunks or participate in hive
//! - **Bootnode**: Topology-only mode. Helps peers discover each other but doesn't
//!   store data. Useful for running dedicated bootstrap infrastructure.
//!
//! # Example
//!
//! ```ignore
//! use vertex_net_topology::{Topology, TopologyConfig};
//! use vertex_swarmspec::init_mainnet;
//!
//! let spec = init_mainnet();
//! let config = TopologyConfig::default();
//!
//! // In bootnode mode, only topology is active
//! let topology = Topology::new(overlay_address, config, spec.bootnodes.clone());
//! topology.connect_bootnodes().await?;
//! ```

#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]

pub mod bootnode;
pub mod hive;
pub mod kademlia;
pub mod manager;

mod error;

pub use error::{TopologyError, TopologyResult};

// Re-export key types
pub use bootnode::BootnodeConnector;
pub use kademlia::{Kademlia, KademliaConfig};
pub use manager::{PeerManager, PeerState};

use vertex_primitives::OverlayAddress;

/// Configuration for the topology system.
#[derive(Debug, Clone)]
pub struct TopologyConfig {
    /// Maximum number of peers to maintain
    pub max_peers: usize,

    /// Target number of peers per bin
    pub bin_saturation: usize,

    /// Whether to participate in the hive protocol (peer exchange)
    pub hive_enabled: bool,

    /// Bootnode-only mode: don't expect storage/retrieval, just maintain topology
    pub bootnode_mode: bool,
}

impl Default for TopologyConfig {
    fn default() -> Self {
        Self {
            max_peers: 50,
            bin_saturation: 4,
            hive_enabled: true,
            bootnode_mode: false,
        }
    }
}

impl TopologyConfig {
    /// Create a configuration for bootnode-only operation.
    ///
    /// Bootnodes maintain topology and help peers discover each other,
    /// but don't participate in storage or retrieval.
    pub fn bootnode() -> Self {
        Self {
            max_peers: 100, // Bootnodes typically handle more connections
            bin_saturation: 4,
            hive_enabled: true,
            bootnode_mode: true,
        }
    }
}

/// The main topology handle.
///
/// This is the entry point for topology operations. It coordinates the kademlia
/// routing table, hive protocol, and peer management.
pub struct Topology {
    /// Local node's overlay address
    overlay: OverlayAddress,

    /// Configuration
    config: TopologyConfig,

    /// Kademlia routing table
    kademlia: Kademlia,
}

impl Topology {
    /// Create a new topology instance.
    pub fn new(overlay: OverlayAddress, config: TopologyConfig) -> Self {
        let kademlia = Kademlia::new(overlay.clone(), KademliaConfig::default());

        Self {
            overlay,
            config,
            kademlia,
        }
    }

    /// Get the local overlay address.
    pub fn overlay(&self) -> &OverlayAddress {
        &self.overlay
    }

    /// Get the kademlia routing table.
    pub fn kademlia(&self) -> &Kademlia {
        &self.kademlia
    }

    /// Get mutable access to the kademlia routing table.
    pub fn kademlia_mut(&mut self) -> &mut Kademlia {
        &mut self.kademlia
    }

    /// Check if this topology is in bootnode-only mode.
    pub fn is_bootnode_mode(&self) -> bool {
        self.config.bootnode_mode
    }

    /// Get the current depth (radius) of the node.
    ///
    /// The depth determines which chunks this node is responsible for storing.
    /// In bootnode mode, this is informational only.
    pub fn depth(&self) -> u8 {
        self.kademlia.depth()
    }
}
