//! The postage domain's contribution to the unified contract indexer, behind
//! the `chain` feature: the PostageStamp tag, address, event ABIs, and the
//! [`PostageReducer`] that folds the batch lifecycle into the
//! [`Batches`](crate::store) projection shared with
//! [`DbBatchStore`](crate::DbBatchStore).

mod abi;
mod canonical;
mod reducer;
mod views;

#[cfg(test)]
mod tests;

pub use reducer::PostageReducer;
pub use views::total_out_payment_at;

use alloy_sol_types::SolEvent;
use vertex_chain_index_framework::{
    ContractTag, DomainRegistration, EventDescriptor, Network, WatchedContract,
};
use vertex_storage::{Database, Table};

use crate::store::Batches;

/// The PostageStamp contract tag, stable across releases (the on-disk
/// [`EventKey`](vertex_chain_index_framework::EventKey) prefix).
pub const TAG_POSTAGE: ContractTag = ContractTag(0x00);

const POSTAGE_EVENTS: &[EventDescriptor] = &[
    EventDescriptor {
        topic0: abi::events::BatchCreated::SIGNATURE_HASH,
        name: "BatchCreated",
    },
    EventDescriptor {
        topic0: abi::events::BatchTopUp::SIGNATURE_HASH,
        name: "BatchTopUp",
    },
    EventDescriptor {
        topic0: abi::events::BatchDepthIncrease::SIGNATURE_HASH,
        name: "BatchDepthIncrease",
    },
    EventDescriptor {
        topic0: abi::events::PriceUpdate::SIGNATURE_HASH,
        name: "PriceUpdate",
    },
];

/// The postage registration for `network`: the watched PostageStamp contract,
/// the [`PostageReducer`], and the shared [`Batches`] table.
pub fn registration<DB: Database>(network: Network) -> DomainRegistration<DB> {
    let deployment = match network {
        Network::Mainnet => canonical::MAINNET,
        Network::Testnet => canonical::TESTNET,
    };

    DomainRegistration {
        contracts: vec![WatchedContract {
            tag: TAG_POSTAGE,
            address: deployment.address,
            start_block: deployment.block,
            events: POSTAGE_EVENTS,
        }],
        reducers: vec![Box::new(PostageReducer)],
        tables: &[Batches::NAME],
    }
}
