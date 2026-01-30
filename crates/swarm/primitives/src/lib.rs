//! Core primitive types for Ethereum Swarm nodes.
//!
//! This crate provides fundamental types used across the Swarm stack,
//! kept separate to avoid circular dependencies.

/// Swarm node type determines what capabilities and protocols the node runs.
///
/// Each type builds on the capabilities of the previous:
/// - Bootnode: Only topology (Hive/Kademlia)
/// - Light: + Bandwidth accounting + Retrieval
/// - Publisher: + Upload/Postage
/// - Full: + Pullsync + Local storage
/// - Staker: + Redistribution game
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash, strum::Display, strum::FromRepr)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
#[strum(serialize_all = "lowercase")]
#[repr(u8)]
pub enum SwarmNodeType {
    /// Bootnode - only participates in topology (Kademlia/Hive).
    Bootnode = 0,

    /// Light node - can retrieve chunks from the network.
    #[default]
    Light = 1,

    /// Publisher node - can retrieve + upload chunks.
    Publisher = 2,

    /// Full node - stores chunks for the network.
    Full = 3,

    /// Staker node - full storage with redistribution rewards.
    Staker = 4,
}

impl SwarmNodeType {
    /// Check if this node type requires availability accounting.
    pub fn requires_availability(&self) -> bool {
        !matches!(self, SwarmNodeType::Bootnode)
    }

    /// Check if this node type requires retrieval protocol.
    pub fn requires_retrieval(&self) -> bool {
        !matches!(self, SwarmNodeType::Bootnode)
    }

    /// Check if this node type requires upload/postage.
    pub fn requires_upload(&self) -> bool {
        matches!(
            self,
            SwarmNodeType::Publisher | SwarmNodeType::Full | SwarmNodeType::Staker
        )
    }

    /// Check if this node type requires pullsync.
    pub fn requires_pullsync(&self) -> bool {
        matches!(self, SwarmNodeType::Full | SwarmNodeType::Staker)
    }

    /// Check if this node type requires local storage.
    pub fn requires_storage(&self) -> bool {
        matches!(self, SwarmNodeType::Full | SwarmNodeType::Staker)
    }

    /// Check if this node type requires redistribution.
    pub fn requires_redistribution(&self) -> bool {
        matches!(self, SwarmNodeType::Staker)
    }

    /// Check if this node type requires persistent identity.
    pub fn requires_persistent_identity(&self) -> bool {
        matches!(self, SwarmNodeType::Full | SwarmNodeType::Staker)
    }

    /// Check if this node type requires persistent nonce (stable overlay).
    pub fn requires_persistent_nonce(&self) -> bool {
        matches!(self, SwarmNodeType::Full | SwarmNodeType::Staker)
    }
}

/// Bandwidth accounting mode.
///
/// Determines how bandwidth costs are tracked and settled between peers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash, strum::Display, strum::FromRepr)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
#[strum(serialize_all = "lowercase")]
#[repr(u8)]
pub enum BandwidthMode {
    /// No bandwidth accounting (dev/testing only).
    ///
    /// Peers freely exchange data without tracking costs.
    /// **Not recommended for production.**
    None = 0,

    /// Soft accounting without real payments (default).
    ///
    /// Tracks bandwidth usage between peers and periodically "settles"
    /// by resetting balances. No actual tokens change hands.
    #[default]
    Pseudosettle = 1,

    /// Real payment channels with chequebook.
    ///
    /// Uses SWAP protocol to issue cheques that can be cashed on-chain.
    /// Requires a funded chequebook contract.
    Swap = 2,

    /// Both pseudosettle and SWAP.
    ///
    /// Uses pseudosettle for soft accounting, with SWAP payments
    /// triggered when thresholds are reached.
    Both = 3,
}

impl BandwidthMode {
    /// Check if pseudosettle is enabled for this mode.
    pub fn pseudosettle_enabled(self) -> bool {
        matches!(self, BandwidthMode::Pseudosettle | BandwidthMode::Both)
    }

    /// Check if SWAP is enabled for this mode.
    pub fn swap_enabled(self) -> bool {
        matches!(self, BandwidthMode::Swap | BandwidthMode::Both)
    }

    /// Check if any bandwidth incentive is enabled.
    pub fn is_enabled(self) -> bool {
        !matches!(self, BandwidthMode::None)
    }
}
