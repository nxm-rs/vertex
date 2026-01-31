//! Core primitive types for Ethereum Swarm nodes.
//!
//! This crate provides fundamental types used across the Swarm stack:
//!
//! - [`OverlayAddress`] - Swarm overlay address for routing and peer identification
//! - [`SwarmNodeType`] - Node type determining capabilities (Bootnode/Client/Storer)
//! - [`BandwidthMode`] - Bandwidth accounting mode (None/Pseudosettle/Swap)
//! - [`ValidatedChunk`] - Type-safe wrapper proving chunk validation

#![cfg_attr(not(feature = "std"), no_std)]

mod validated;

pub use validated::{ValidatedChunk, ValidationError};

use alloy_primitives::{Address, B256, Keccak256};
use nectar_primitives::SwarmAddress;

/// Overlay address for Swarm routing and peer identification.
///
/// A 32-byte address derived from `keccak256(ethereum_address || network_id || nonce)`.
///
/// Used for Kademlia routing, bandwidth accounting, chunk sync, and topology.
/// All swarm-api traits use `OverlayAddress`. The net/ layer handles the
/// mapping to libp2p's `PeerId` for actual network connections.
pub type OverlayAddress = SwarmAddress;

/// Computes overlay address: `keccak256(ethereum_address || network_id || nonce)`.
pub fn compute_overlay(ethereum_address: &Address, network_id: u64, nonce: &B256) -> SwarmAddress {
    let mut hasher = Keccak256::new();
    hasher.update(ethereum_address);
    hasher.update(network_id.to_le_bytes());
    hasher.update(nonce);
    hasher.finalize().into()
}

/// Swarm node type determining capabilities and protocols.
///
/// Hierarchy: Bootnode < Client < Storer (each level adds capabilities).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash, strum::Display, strum::FromRepr)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
#[strum(serialize_all = "lowercase")]
#[repr(u8)]
pub enum SwarmNodeType {
    /// Topology only: handshake, hive, pingpong. No pricing or accounting.
    Bootnode = 0,

    /// Read + write: retrieval, pushsync, pricing, configurable accounting.
    #[default]
    Client = 1,

    /// Storage + staking: pullsync, local storage, runtime staking toggle.
    Storer = 2,
}

impl SwarmNodeType {
    /// Returns true if this node type participates in pricing protocol.
    pub fn requires_pricing(&self) -> bool {
        !matches!(self, SwarmNodeType::Bootnode)
    }

    /// Returns true if this node type tracks bandwidth costs.
    pub fn requires_accounting(&self) -> bool {
        !matches!(self, SwarmNodeType::Bootnode)
    }

    /// Returns true if this node type can retrieve chunks.
    pub fn requires_retrieval(&self) -> bool {
        !matches!(self, SwarmNodeType::Bootnode)
    }

    /// Returns true if this node type can push chunks.
    pub fn requires_pushsync(&self) -> bool {
        !matches!(self, SwarmNodeType::Bootnode)
    }

    /// Returns true if this node type syncs chunks from neighbors.
    pub fn requires_pullsync(&self) -> bool {
        matches!(self, SwarmNodeType::Storer)
    }

    /// Returns true if this node type stores chunks locally.
    pub fn requires_storage(&self) -> bool {
        matches!(self, SwarmNodeType::Storer)
    }

    /// Returns true if this node type can stake (capability, not state).
    pub fn supports_staking(&self) -> bool {
        matches!(self, SwarmNodeType::Storer)
    }

    /// Returns true if this node type needs persistent identity.
    pub fn requires_persistent_identity(&self) -> bool {
        matches!(self, SwarmNodeType::Storer)
    }

    /// Returns true if this node type needs stable overlay address.
    pub fn requires_persistent_nonce(&self) -> bool {
        matches!(self, SwarmNodeType::Storer)
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
    /// Check if pseudosettle is enabled for this mode.
    pub fn pseudosettle_enabled(self) -> bool {
        matches!(self, BandwidthMode::Pseudosettle | BandwidthMode::Both)
    }

    /// Check if SWAP is enabled for this mode.
    pub fn swap_enabled(self) -> bool {
        matches!(self, BandwidthMode::Swap | BandwidthMode::Both)
    }

    /// Check if bandwidth accounting is enabled.
    pub fn is_enabled(self) -> bool {
        !matches!(self, BandwidthMode::None)
    }
}
