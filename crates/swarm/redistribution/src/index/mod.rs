//! On-chain indexing for the two storage-incentives contracts, Redistribution
//! and StakeRegistry, in one crate.
//!
//! Both are lazy domains: events land verbatim in the framework's
//! [`EventTable`](vertex_chain_index_framework::EventTable) and the
//! [`redistribution`]/[`staking`] views fold on read. No reducers, no projection
//! tables.

use alloy_sol_types::SolEvent;
use nectar_contracts::IStakeRegistry;
use vertex_chain_index_framework::{
    ContractTag, DomainRegistration, EventDescriptor, Network, WatchedContract,
};
use vertex_storage::Database;

pub mod redistribution;
pub mod staking;

mod canonical;

/// Redistribution contract tag. Stable: part of the on-disk
/// [`EventKey`](vertex_chain_index_framework::EventKey) prefix.
pub const TAG_REDISTRIBUTION: ContractTag = ContractTag(0x02);

/// StakeRegistry contract tag. Stable: part of the on-disk
/// [`EventKey`](vertex_chain_index_framework::EventKey) prefix.
pub const TAG_STAKING: ContractTag = ContractTag(0x01);

/// Event ABIs nectar does not yet ship: the Redistribution set and
/// StakeRegistry's `OverlayChanged`. Each domain owns the `sol!` events it
/// decodes.
pub mod events {
    use alloy_sol_types::sol;

    sol! {
        /// A node's overlay moved. Both `owner` and `overlay` are non-indexed.
        #[allow(missing_docs)]
        event OverlayChanged(address owner, bytes32 overlay);

        /// A node committed its obfuscated reveal hash for `roundNumber`.
        #[allow(missing_docs)]
        event Committed(uint256 roundNumber, bytes32 overlay, uint8 height);

        /// A node revealed its reserve commitment for `roundNumber`.
        #[allow(missing_docs)]
        event Revealed(
            uint256 roundNumber,
            bytes32 overlay,
            uint256 stake,
            uint256 stakeDensity,
            bytes32 reserveCommitment,
            uint8 depth
        );

        /// The reveal anchor (the seed truth selection draws against) for
        /// `roundNumber`.
        #[allow(missing_docs)]
        event CurrentRevealAnchor(uint256 roundNumber, bytes32 anchor);

        /// The truth (the agreed reserve hash and depth) the round settled on.
        #[allow(missing_docs)]
        event TruthSelected(bytes32 hash, uint8 depth);

        /// A reveal record, as carried by [`WinnerSelected`].
        #[allow(missing_docs)]
        struct Reveal {
            bytes32 overlay;
            address owner;
            uint8 depth;
            uint256 stake;
            uint256 stakeDensity;
            bytes32 hash;
        }

        /// The winning reveal a round paid out to.
        #[allow(missing_docs)]
        event WinnerSelected(Reveal winner);

        /// The number of commits counted in the current round.
        #[allow(missing_docs)]
        event CountCommits(uint256 _count);

        /// The number of reveals counted in the current round.
        #[allow(missing_docs)]
        event CountReveals(uint256 _count);

        /// The valid chunk count the round priced against.
        #[allow(missing_docs)]
        event ChunkCount(uint256 validChunkCount);
    }
}

const REDISTRIBUTION_EVENTS: &[EventDescriptor] = &[
    EventDescriptor {
        topic0: events::Committed::SIGNATURE_HASH,
        name: "Committed",
    },
    EventDescriptor {
        topic0: events::Revealed::SIGNATURE_HASH,
        name: "Revealed",
    },
    EventDescriptor {
        topic0: events::CurrentRevealAnchor::SIGNATURE_HASH,
        name: "CurrentRevealAnchor",
    },
    EventDescriptor {
        topic0: events::TruthSelected::SIGNATURE_HASH,
        name: "TruthSelected",
    },
    EventDescriptor {
        topic0: events::WinnerSelected::SIGNATURE_HASH,
        name: "WinnerSelected",
    },
    EventDescriptor {
        topic0: events::CountCommits::SIGNATURE_HASH,
        name: "CountCommits",
    },
    EventDescriptor {
        topic0: events::CountReveals::SIGNATURE_HASH,
        name: "CountReveals",
    },
    EventDescriptor {
        topic0: events::ChunkCount::SIGNATURE_HASH,
        name: "ChunkCount",
    },
];

const STAKING_EVENTS: &[EventDescriptor] = &[
    EventDescriptor {
        topic0: IStakeRegistry::StakeUpdated::SIGNATURE_HASH,
        name: "StakeUpdated",
    },
    EventDescriptor {
        topic0: IStakeRegistry::StakeFrozen::SIGNATURE_HASH,
        name: "StakeFrozen",
    },
    EventDescriptor {
        topic0: IStakeRegistry::StakeSlashed::SIGNATURE_HASH,
        name: "StakeSlashed",
    },
    EventDescriptor {
        topic0: IStakeRegistry::StakeWithdrawn::SIGNATURE_HASH,
        name: "StakeWithdrawn",
    },
    EventDescriptor {
        topic0: events::OverlayChanged::SIGNATURE_HASH,
        name: "OverlayChanged",
    },
];

/// The two watched contracts for `network`, with no reducers and no tables.
pub fn registration<DB: Database>(network: Network) -> DomainRegistration<DB> {
    let (redistribution, staking) = match network {
        Network::Mainnet => (
            canonical::mainnet::REDISTRIBUTION,
            canonical::mainnet::STAKING,
        ),
        Network::Testnet => (
            canonical::testnet::REDISTRIBUTION,
            canonical::testnet::STAKING,
        ),
    };

    DomainRegistration {
        contracts: vec![
            WatchedContract {
                tag: TAG_REDISTRIBUTION,
                address: redistribution.address,
                start_block: redistribution.block,
                events: REDISTRIBUTION_EVENTS,
            },
            WatchedContract {
                tag: TAG_STAKING,
                address: staking.address,
                start_block: staking.block,
                events: STAKING_EVENTS,
            },
        ],
        reducers: vec![],
        tables: &[],
    }
}

#[cfg(test)]
mod tests;
