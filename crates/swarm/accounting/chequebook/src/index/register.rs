//! The chequebook domain's registration: its tag and single watched factory contract.

use alloy_sol_types::SolEvent;
use nectar_contracts::IChequebookFactory;
use vertex_chain_index_framework::{
    ContractTag, DomainRegistration, EventDescriptor, Network, WatchedContract,
};
use vertex_storage::Database;

use crate::index::canonical;

/// The chequebook-factory contract tag. Stable across releases (part of the
/// on-disk [`EventKey`](vertex_chain_index_framework::EventKey) byte format).
pub const TAG_CHEQUEBOOK: ContractTag = ContractTag(0x03);

const CHEQUEBOOK_EVENTS: &[EventDescriptor] = &[EventDescriptor {
    topic0: IChequebookFactory::SimpleSwapDeployed::SIGNATURE_HASH,
    name: "SimpleSwapDeployed",
}];

/// Build the chequebook domain's registration for `network`.
pub fn registration<DB: Database>(network: Network) -> DomainRegistration<DB> {
    let factory = match network {
        Network::Mainnet => canonical::mainnet::CHEQUEBOOK_FACTORY,
        Network::Testnet => canonical::testnet::CHEQUEBOOK_FACTORY,
    };

    DomainRegistration {
        contracts: vec![WatchedContract {
            tag: TAG_CHEQUEBOOK,
            address: factory.address,
            start_block: factory.block,
            events: CHEQUEBOOK_EVENTS,
        }],
        reducers: vec![],
        tables: &[],
    }
}
