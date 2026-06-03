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

    pub fn requires_persistent_identity(&self) -> bool {
        matches!(self, SwarmNodeType::Storer)
    }

    pub fn requires_persistent_nonce(&self) -> bool {
        matches!(self, SwarmNodeType::Storer)
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
