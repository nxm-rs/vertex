//! The swap-price-oracle domain's registration: tag, watched contract, and the
//! [`DomainRegistration`] the node builder collects into the unified indexer.

use alloy_sol_types::SolEvent;
use nectar_contracts::ISwapPriceOracle;
use vertex_chain_index_framework::{
    ContractTag, DomainRegistration, EventDescriptor, Network, WatchedContract,
};
use vertex_storage::Database;

/// The swap price-oracle contract tag. Stable across releases (part of the
/// on-disk [`EventKey`](vertex_chain_index_framework::EventKey) format).
pub const TAG_SWAP_ORACLE: ContractTag = ContractTag(0x04);

const SWAP_EVENTS: &[EventDescriptor] = &[
    EventDescriptor {
        topic0: ISwapPriceOracle::PriceUpdate::SIGNATURE_HASH,
        name: "PriceUpdate",
    },
    EventDescriptor {
        topic0: ISwapPriceOracle::ChequeValueDeductionUpdate::SIGNATURE_HASH,
        name: "ChequeValueDeductionUpdate",
    },
];

/// Build the swap domain's registration for `network`.
pub fn registration<DB: Database>(network: Network) -> DomainRegistration<DB> {
    let swap = match network {
        Network::Mainnet => nectar_contracts::mainnet::SWAP_PRICE_ORACLE,
        Network::Testnet => nectar_contracts::testnet::SWAP_PRICE_ORACLE,
    };

    DomainRegistration {
        contracts: vec![WatchedContract {
            tag: TAG_SWAP_ORACLE,
            address: swap.address,
            start_block: swap.block,
            events: SWAP_EVENTS,
        }],
        reducers: vec![],
        tables: &[],
    }
}
