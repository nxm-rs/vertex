//! Core primitive types for Ethereum Swarm nodes.
//!
//! This crate provides fundamental types used across the Swarm stack,
//! kept separate to avoid circular dependencies.

use alloy_primitives::{Address, B256, Keccak256};
use nectar_primitives::SwarmAddress;

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
/// Hierarchy: Bootnode < Client < Storer (each adds capabilities).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash, strum::Display, strum::FromRepr)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
#[strum(serialize_all = "lowercase")]
#[repr(u8)]
pub enum SwarmNodeType {
    /// Topology only: handshake, hive, pingpong. No pricing or accounting.
    Bootnode = 0,

    /// Read + write: retrieval, pushsync, pricing, configurable accounting.
    /// Consumes the swarm network without storing chunks locally.
    #[default]
    Client = 1,

    /// Storage + staking: pullsync, local storage, runtime staking toggle.
    /// Stores chunks locally and participates in the storage incentive game.
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
