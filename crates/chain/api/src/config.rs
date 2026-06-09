//! Chain address configuration.
//!
//! [`ChainConfig`] is the address book a chain consumer needs: the settlement
//! chain plus the contract addresses the node interacts with. It is pure data.
//! A consumer builds an `alloy_provider::Provider` from a transport and reads
//! these addresses to target the contracts.
//!
//! The chain is an [`alloy_chains::NamedChain`], not a bare integer, so the
//! EIP-155 id, the chain's name, and its membership in helper sets all come from
//! one canonical type rather than a magic number passed around the codebase.
//! Addresses come from the canonical `nectar_contracts` deployment constants.

use alloy_chains::NamedChain;
use alloy_primitives::Address;
use nectar_contracts::{ChequebookFactory, StoragePriceOracle, Token};

/// Contract addresses and the settlement chain for chain access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChainConfig {
    /// The settlement chain (Gnosis for mainnet, Sepolia for testnet).
    pub chain: NamedChain,

    /// Chequebook factory (SimpleSwapFactory) address.
    pub chequebook_factory: Address,

    /// BZZ token (ERC20) address.
    pub bzz_token: Address,

    /// Storage price oracle address.
    pub price_oracle: Address,
}

impl ChainConfig {
    /// Build a config from a chain and explicit contract addresses.
    pub fn new(
        chain: NamedChain,
        chequebook_factory: Address,
        bzz_token: Address,
        price_oracle: Address,
    ) -> Self {
        Self {
            chain,
            chequebook_factory,
            bzz_token,
            price_oracle,
        }
    }

    /// Build a config from `nectar_contracts` deployment structs and a chain.
    ///
    /// Lets a consumer pass the canonical `mainnet::*` / `testnet::*` deployment
    /// constants directly rather than peeling out each `.address`.
    pub fn from_deployments(
        chain: NamedChain,
        chequebook_factory: ChequebookFactory,
        bzz_token: Token,
        price_oracle: StoragePriceOracle,
    ) -> Self {
        Self::new(
            chain,
            chequebook_factory.address,
            bzz_token.address,
            price_oracle.address,
        )
    }

    /// Gnosis Chain mainnet addresses.
    pub fn mainnet() -> Self {
        use nectar_contracts::mainnet;
        Self::from_deployments(
            NamedChain::Gnosis,
            mainnet::CHEQUEBOOK_FACTORY,
            mainnet::BZZ_TOKEN,
            mainnet::STORAGE_PRICE_ORACLE,
        )
    }

    /// Sepolia testnet addresses.
    pub fn testnet() -> Self {
        use nectar_contracts::testnet;
        Self::from_deployments(
            NamedChain::Sepolia,
            testnet::CHEQUEBOOK_FACTORY,
            testnet::BZZ_TOKEN,
            testnet::STORAGE_PRICE_ORACLE,
        )
    }
}
