//! Chain address configuration.
//!
//! [`ChainConfig`] is the address book a chain service needs: the chain id plus
//! the contract addresses the node interacts with. It is pure data so it lives
//! in the wasm-safe api crate; the service crate consumes it to build a
//! provider.
//!
//! Addresses are supplied explicitly through [`ChainConfig::from_deployments`].
//! Convenience constructors derive them from the canonical `nectar_contracts`
//! deployment constants so a consumer that already knows it is on mainnet or
//! testnet does not restate the addresses. Deriving from a full network spec is
//! left to the consumer so this crate stays free of the spec dependency.

use alloy_primitives::Address;
use nectar_contracts::{ChequebookFactory, StoragePriceOracle, Token};

/// Contract addresses and chain identity for a chain service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChainConfig {
    /// EIP-155 chain id (100 for Gnosis mainnet, 11155111 for Sepolia testnet).
    pub chain_id: u64,

    /// Chequebook factory (SimpleSwapFactory) address.
    pub chequebook_factory: Address,

    /// BZZ token (ERC20) address.
    pub bzz_token: Address,

    /// Storage price oracle address.
    pub price_oracle: Address,
}

impl ChainConfig {
    /// Build a config from an explicit chain id and contract addresses.
    pub fn from_deployments(
        chain_id: u64,
        chequebook_factory: Address,
        bzz_token: Address,
        price_oracle: Address,
    ) -> Self {
        Self {
            chain_id,
            chequebook_factory,
            bzz_token,
            price_oracle,
        }
    }

    /// Build a config from `nectar_contracts` deployment structs and a chain id.
    ///
    /// Lets a consumer pass the canonical `mainnet::*` / `testnet::*` deployment
    /// constants directly rather than peeling out each `.address`.
    pub fn from_deployment_structs(
        chain_id: u64,
        chequebook_factory: ChequebookFactory,
        bzz_token: Token,
        price_oracle: StoragePriceOracle,
    ) -> Self {
        Self::from_deployments(
            chain_id,
            chequebook_factory.address,
            bzz_token.address,
            price_oracle.address,
        )
    }

    /// Gnosis Chain mainnet addresses.
    pub fn mainnet() -> Self {
        use nectar_contracts::mainnet;
        Self::from_deployment_structs(
            100,
            mainnet::CHEQUEBOOK_FACTORY,
            mainnet::BZZ_TOKEN,
            mainnet::STORAGE_PRICE_ORACLE,
        )
    }

    /// Sepolia testnet addresses.
    pub fn testnet() -> Self {
        use nectar_contracts::testnet;
        Self::from_deployment_structs(
            11155111,
            testnet::CHEQUEBOOK_FACTORY,
            testnet::BZZ_TOKEN,
            testnet::STORAGE_PRICE_ORACLE,
        )
    }
}
