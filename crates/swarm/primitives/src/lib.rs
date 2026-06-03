//! Core primitive types for Ethereum Swarm nodes.
//!
//! This crate re-exports canonical Swarm primitives from `nectar-primitives`
//! and adds vertex-specific node configuration types.

#![cfg_attr(not(feature = "std"), no_std)]

mod validated;

pub use validated::{ValidatedChunk, ValidationError};

// Re-export canonical Swarm primitives from nectar.
pub use nectar_primitives::{Bin, NetworkId, Nonce, ProximityOrder, Timestamp, compute_overlay};

use nectar_primitives::SwarmAddress;

/// Overlay address for Swarm routing and peer identification.
pub type OverlayAddress = SwarmAddress;

/// Swarm node type determining capabilities and protocols.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    Hash,
    strum::Display,
    strum::FromRepr,
    strum::IntoStaticStr,
)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
#[strum(serialize_all = "lowercase")]
#[repr(u8)]
pub enum SwarmNodeType {
    /// Topology only: handshake, hive, pingpong.
    Bootnode = 0,
    /// Read + write: retrieval, pushsync, pricing.
    #[default]
    Client = 1,
    /// Storage + staking: pullsync, local storage.
    Storer = 2,
}

impl SwarmNodeType {
    pub fn requires_pricing(&self) -> bool {
        !matches!(self, SwarmNodeType::Bootnode)
    }

    pub fn requires_accounting(&self) -> bool {
        !matches!(self, SwarmNodeType::Bootnode)
    }

    pub fn requires_retrieval(&self) -> bool {
        !matches!(self, SwarmNodeType::Bootnode)
    }

    pub fn requires_pushsync(&self) -> bool {
        !matches!(self, SwarmNodeType::Bootnode)
    }

    pub fn requires_pullsync(&self) -> bool {
        matches!(self, SwarmNodeType::Storer)
    }

    pub fn requires_storage(&self) -> bool {
        matches!(self, SwarmNodeType::Storer)
    }

    pub fn supports_staking(&self) -> bool {
        matches!(self, SwarmNodeType::Storer)
    }

    /// Whether this node type requires a persistent (keystore-backed) signing
    /// key. Bootnodes need a stable overlay address across restarts so peers
    /// can reach them via their well-known overlay. Storers need a stable key
    /// for staking and chunk reservation.
    pub fn requires_persistent_identity(&self) -> bool {
        matches!(self, SwarmNodeType::Bootnode | SwarmNodeType::Storer)
    }

    /// Whether this node type requires a persistent nonce. Combined with a
    /// persistent signing key, this fixes the overlay address across restarts.
    pub fn requires_persistent_nonce(&self) -> bool {
        matches!(self, SwarmNodeType::Bootnode | SwarmNodeType::Storer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootnode_requires_persistent_identity_and_nonce() {
        // Bootnode overlay is a contract: peers reach it via a well-known
        // overlay address derived from the keystore key and nonce. Both must
        // survive restart.
        assert!(SwarmNodeType::Bootnode.requires_persistent_identity());
        assert!(SwarmNodeType::Bootnode.requires_persistent_nonce());
    }

    #[test]
    fn storer_requires_persistent_identity_and_nonce() {
        assert!(SwarmNodeType::Storer.requires_persistent_identity());
        assert!(SwarmNodeType::Storer.requires_persistent_nonce());
    }

    #[test]
    fn client_does_not_require_persistence() {
        assert!(!SwarmNodeType::Client.requires_persistent_identity());
        assert!(!SwarmNodeType::Client.requires_persistent_nonce());
    }

    #[test]
    fn bootnode_excludes_client_protocols() {
        let t = SwarmNodeType::Bootnode;
        assert!(!t.requires_pricing());
        assert!(!t.requires_accounting());
        assert!(!t.requires_retrieval());
        assert!(!t.requires_pushsync());
        assert!(!t.requires_pullsync());
        assert!(!t.requires_storage());
        assert!(!t.supports_staking());
    }
}

/// Bandwidth accounting mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash, strum::Display, strum::FromRepr)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
#[strum(serialize_all = "lowercase")]
#[repr(u8)]
pub enum BandwidthMode {
    /// No bandwidth accounting (dev/testing only).
    None = 0,
    /// Soft accounting without real payments (default).
    #[default]
    Pseudosettle = 1,
    /// Real payment channels with chequebook.
    Swap = 2,
    /// Both pseudosettle and SWAP.
    Both = 3,
}

impl BandwidthMode {
    pub fn pseudosettle_enabled(self) -> bool {
        matches!(self, BandwidthMode::Pseudosettle | BandwidthMode::Both)
    }

    pub fn swap_enabled(self) -> bool {
        matches!(self, BandwidthMode::Swap | BandwidthMode::Both)
    }

    pub fn is_enabled(self) -> bool {
        !matches!(self, BandwidthMode::None)
    }
}
