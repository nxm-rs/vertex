//! Smart contract definitions for Swarm

use alloy_primitives::{Address, B256};
use serde::{Deserialize, Serialize};

/// Trait for contract addresses on chain
pub trait ContractAddress {
    /// Returns the contract's deployed address
    fn address(&self) -> Address;

    /// Returns the block number when the contract was deployed
    fn deployment_block(&self) -> u64;

    /// Returns the transaction hash of the deployment transaction
    fn deployment_tx(&self) -> B256;
}

/// Swarm token (BZZ) contract details
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwarmToken {
    /// Contract address
    pub address: Address,
    /// Token name
    pub name: &'static str,
    /// Token symbol
    pub symbol: &'static str,
    /// Decimal places
    pub decimals: u8,
}

impl ContractAddress for SwarmToken {
    fn address(&self) -> Address {
        self.address
    }

    fn deployment_block(&self) -> u64 {
        0 // Placeholder, actual deployment block would be set in implementation
    }

    fn deployment_tx(&self) -> B256 {
        B256::ZERO // Placeholder
    }
}

/// Postage stamp contract details
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostageContract {
    /// Contract address
    pub address: Address,
    /// Deployment block number
    pub deployment_block: u64,
    /// Deployment transaction hash
    pub deployment_tx: B256,
}

impl ContractAddress for PostageContract {
    fn address(&self) -> Address {
        self.address
    }

    fn deployment_block(&self) -> u64 {
        self.deployment_block
    }

    fn deployment_tx(&self) -> B256 {
        self.deployment_tx
    }
}

/// Redistribution contract details
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedistributionContract {
    /// Contract address
    pub address: Address,
    /// Deployment block number
    pub deployment_block: u64,
    /// Deployment transaction hash
    pub deployment_tx: B256,
}

impl ContractAddress for RedistributionContract {
    fn address(&self) -> Address {
        self.address
    }

    fn deployment_block(&self) -> u64 {
        self.deployment_block
    }

    fn deployment_tx(&self) -> B256 {
        self.deployment_tx
    }
}

/// Staking contract details
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StakingContract {
    /// Contract address
    pub address: Address,
    /// Deployment block number
    pub deployment_block: u64,
    /// Deployment transaction hash
    pub deployment_tx: B256,
}

impl ContractAddress for StakingContract {
    fn address(&self) -> Address {
        self.address
    }

    fn deployment_block(&self) -> u64 {
        self.deployment_block
    }

    fn deployment_tx(&self) -> B256 {
        self.deployment_tx
    }
}
