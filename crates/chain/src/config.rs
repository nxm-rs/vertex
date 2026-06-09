//! Chain address configuration.
//!
//! [`ChainConfig`] is the address book a chain consumer needs: the settlement
//! chain plus the contract addresses the node interacts with. It is pure data.
//! A consumer builds an `alloy_provider::Provider` from a transport and reads
//! these addresses to target the contracts.
//!
//! The network-to-chain mapping is not reinvented here. A Swarm network is a
//! [`nectar_swarms::NamedSwarm`], and the settlement chain it runs on comes from
//! [`NamedSwarm::chain`]. The contract addresses are not part of that mapping;
//! they come from the canonical `nectar_contracts` deployment constants for the
//! matching network. So [`ChainConfig::for_swarm`] pairs the two: the chain from
//! `nectar_swarms`, the addresses from `nectar_contracts`.

use alloy_chains::Chain;
use alloy_primitives::Address;
use nectar_contracts::{ChequebookFactory, StoragePriceOracle, Token};
use nectar_swarms::{NamedSwarm, Swarm};

/// Contract addresses and the settlement chain for chain access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChainConfig {
    /// The settlement chain (Gnosis for mainnet, Sepolia for testnet).
    pub chain: Chain,

    /// Chequebook factory (SimpleSwapFactory) address.
    pub chequebook_factory: Address,

    /// BZZ token (ERC20) address.
    pub bzz_token: Address,

    /// Storage price oracle address.
    pub price_oracle: Address,
}

impl ChainConfig {
    /// Build a config from a chain and explicit contract addresses.
    pub const fn new(
        chain: Chain,
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
    pub const fn from_deployments(
        chain: Chain,
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

    /// Build a config for a named Swarm network.
    ///
    /// The settlement chain comes from [`NamedSwarm::chain`] and the contract
    /// addresses from the matching `nectar_contracts` deployment. Returns `None`
    /// for networks with no canonical deployment, such as [`NamedSwarm::Dev`].
    pub fn for_swarm(swarm: NamedSwarm) -> Option<Self> {
        match swarm {
            NamedSwarm::Mainnet => Some(Self::mainnet()),
            NamedSwarm::Testnet => Some(Self::testnet()),
            // Dev and any future network with no canonical deployment.
            _ => None,
        }
    }

    /// Build a config for a [`Swarm`].
    ///
    /// Returns `None` for custom networks and any named network with no canonical
    /// deployment. See [`ChainConfig::for_swarm`].
    pub fn from_swarm(swarm: Swarm) -> Option<Self> {
        Self::for_swarm(swarm.named()?)
    }

    /// Gnosis Chain mainnet addresses, with the settlement chain from
    /// [`NamedSwarm::Mainnet`].
    pub fn mainnet() -> Self {
        use nectar_contracts::mainnet;
        Self::from_deployments(
            NamedSwarm::Mainnet.chain(),
            mainnet::CHEQUEBOOK_FACTORY,
            mainnet::BZZ_TOKEN,
            mainnet::STORAGE_PRICE_ORACLE,
        )
    }

    /// Sepolia testnet addresses, with the settlement chain from
    /// [`NamedSwarm::Testnet`].
    pub fn testnet() -> Self {
        use nectar_contracts::testnet;
        Self::from_deployments(
            NamedSwarm::Testnet.chain(),
            testnet::CHEQUEBOOK_FACTORY,
            testnet::BZZ_TOKEN,
            testnet::STORAGE_PRICE_ORACLE,
        )
    }
}
