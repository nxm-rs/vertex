//! Contracts as data: the [`WatchedContract`] / [`EventDescriptor`] model and
//! the static [`registry`] built from the canonical nectar address book.
//!
//! A watched contract is a value, not a type. Adding a contract is one
//! [`ContractId`] variant plus one [`WatchedContract`] entry in [`registry`];
//! the indexer, the store, and the views are contract-count-independent.
//!
//! The addresses and deployment blocks come from `nectar_contracts::mainnet` /
//! `nectar_contracts::testnet`, so they live upstream exactly once and Vertex
//! reads them. The event ABIs nectar already ships
//! (`IStoragePriceOracle::PriceUpdate`, `IStakeRegistry::Stake*`,
//! `IChequebookFactory::SimpleSwapDeployed`, `ISwapPriceOracle::*`) are
//! referenced directly; the few ABIs nectar does not yet carry (the PostageStamp
//! batch/price events, `StakeRegistry::OverlayChanged`, and the Redistribution
//! event set) are declared in the local [`abi`] `sol!` block below.
//!
//! Follow-up: upstream the [`abi`] events to `nectar-contracts` so every event
//! ABI lives in one place (primitives belong in nectar per the repo split).

use alloy_primitives::{Address, B256};
use alloy_sol_types::SolEvent;
use nectar_contracts::{IChequebookFactory, IStakeRegistry, IStoragePriceOracle, ISwapPriceOracle};
use serde::{Deserialize, Serialize};
use strum::IntoStaticStr;

/// The event ABIs nectar does not yet ship, declared here verbatim from the
/// deployment manifests.
///
/// These are a candidate to upstream to `nectar-contracts`; until then this is
/// the single place they live, replacing the per-branch re-declarations.
pub mod abi {
    use alloy_sol_types::sol;

    sol! {
        // PostageStamp batch and pricing events.

        /// A new batch was created with its full parameters. `normalisedBalance`
        /// is the level the rising `currentTotalOutPayment` line must stay below
        /// for the batch to remain valid.
        #[allow(missing_docs)]
        event BatchCreated(
            bytes32 indexed batchId,
            uint256 totalAmount,
            uint256 normalisedBalance,
            address owner,
            uint8 depth,
            uint8 bucketDepth,
            bool immutableFlag
        );

        /// An existing batch was topped up, raising its `normalisedBalance`.
        #[allow(missing_docs)]
        event BatchTopUp(bytes32 indexed batchId, uint256 topupAmount, uint256 normalisedBalance);

        /// An existing batch's depth was increased, with a re-normalised balance.
        #[allow(missing_docs)]
        event BatchDepthIncrease(bytes32 indexed batchId, uint8 newDepth, uint256 normalisedBalance);

        /// The per-chunk-per-block storage price was set. The running
        /// `totalOutPayment` accumulator is reconstructed from this cadence.
        #[allow(missing_docs)]
        event PriceUpdate(uint256 price);

        /// The contract was paused (or unpaused) by `account`.
        #[allow(missing_docs)]
        event Paused(address account);

        // StakeRegistry: the one event the shared `IStakeRegistry` interface
        // does not carry.

        /// A node's overlay moved. Both `owner` and `overlay` are non-indexed.
        #[allow(missing_docs)]
        event OverlayChanged(address owner, bytes32 overlay);

        // Redistribution: the full event set, which the shared `IRedistribution`
        // interface does not carry.

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

/// A stable, small enum tag for each watched contract.
///
/// It is the cursor-namespace discriminant in the generic event store
/// ([`crate::store::EventKey`]), the per-contract metric label (via
/// [`IntoStaticStr`]), and the address-resolution target in
/// [`crate::indexer::ContractIndexer::apply`]. `#[non_exhaustive]` so adding a
/// contract is not a breaking change for downstream `match`es.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, IntoStaticStr,
)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum ContractId {
    /// The PostageStamp contract.
    Postage,
    /// The StakeRegistry contract.
    Staking,
    /// The Redistribution contract.
    Redistribution,
    /// The chequebook factory (SimpleSwapFactory).
    ChequebookFactory,
    /// The swap (settlement) price oracle.
    SwapPriceOracle,
    /// The storage price oracle.
    StoragePriceOracle,
}

impl ContractId {
    /// The 1-byte on-disk tag for this contract, the high-order key prefix in
    /// [`crate::store::EventKey`].
    ///
    /// Stable across releases: the byte values are part of the on-disk format,
    /// so reorder variants only by appending. Decoded back through
    /// [`Self::from_tag`].
    pub const fn tag(self) -> u8 {
        match self {
            Self::Postage => 0,
            Self::Staking => 1,
            Self::Redistribution => 2,
            Self::ChequebookFactory => 3,
            Self::SwapPriceOracle => 4,
            Self::StoragePriceOracle => 5,
        }
    }

    /// Decode a [`ContractId`] from its on-disk [`tag`](Self::tag).
    pub const fn from_tag(tag: u8) -> Option<Self> {
        Some(match tag {
            0 => Self::Postage,
            1 => Self::Staking,
            2 => Self::Redistribution,
            3 => Self::ChequebookFactory,
            4 => Self::SwapPriceOracle,
            5 => Self::StoragePriceOracle,
            _ => return None,
        })
    }
}

/// One event a contract emits: its `topic0` and a human label.
///
/// `topic0` is `E::SIGNATURE_HASH` for the concrete `sol!` event; `name` labels
/// the stored row and metrics. The descriptor set is the topic-confusion
/// defence: [`crate::indexer::ContractIndexer::apply`] verifies a log's `topic0`
/// is declared for the contract its address resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventDescriptor {
    /// The event's `topic0` (`E::SIGNATURE_HASH`).
    pub topic0: B256,
    /// A human label for the stored row and metrics.
    pub name: &'static str,
}

/// A contract to watch: its id, address, deployment block, and event set.
///
/// Built from the canonical nectar address book in [`registry`]. The combined
/// [`crate::indexer::ContractIndexer`] filter is the union of every watched
/// contract's address and every descriptor's `topic0`.
#[derive(Debug, Clone, Copy)]
pub struct WatchedContract {
    /// The stable contract tag.
    pub id: ContractId,
    /// The contract address (from nectar mainnet/testnet).
    pub address: Address,
    /// The deployment block (from nectar mainnet/testnet); backfill starts here.
    pub start_block: u64,
    /// The events this contract emits that the indexer records.
    pub events: &'static [EventDescriptor],
}

impl WatchedContract {
    /// Whether `topic0` is an event this contract declares.
    pub fn declares(&self, topic0: B256) -> bool {
        self.events.iter().any(|e| e.topic0 == topic0)
    }
}

/// The settlement network whose address book the [`registry`] is built from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Network {
    /// Gnosis Chain mainnet.
    Mainnet,
    /// Sepolia testnet.
    Testnet,
}

// The per-contract descriptor sets. `const` so the registry is built from
// `&'static` slices with no allocation.

const POSTAGE_EVENTS: &[EventDescriptor] = &[
    EventDescriptor {
        topic0: abi::BatchCreated::SIGNATURE_HASH,
        name: "BatchCreated",
    },
    EventDescriptor {
        topic0: abi::BatchTopUp::SIGNATURE_HASH,
        name: "BatchTopUp",
    },
    EventDescriptor {
        topic0: abi::BatchDepthIncrease::SIGNATURE_HASH,
        name: "BatchDepthIncrease",
    },
    EventDescriptor {
        topic0: abi::PriceUpdate::SIGNATURE_HASH,
        name: "PriceUpdate",
    },
    EventDescriptor {
        topic0: abi::Paused::SIGNATURE_HASH,
        name: "Paused",
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
        topic0: abi::OverlayChanged::SIGNATURE_HASH,
        name: "OverlayChanged",
    },
];

const REDISTRIBUTION_EVENTS: &[EventDescriptor] = &[
    EventDescriptor {
        topic0: abi::Committed::SIGNATURE_HASH,
        name: "Committed",
    },
    EventDescriptor {
        topic0: abi::Revealed::SIGNATURE_HASH,
        name: "Revealed",
    },
    EventDescriptor {
        topic0: abi::CurrentRevealAnchor::SIGNATURE_HASH,
        name: "CurrentRevealAnchor",
    },
    EventDescriptor {
        topic0: abi::TruthSelected::SIGNATURE_HASH,
        name: "TruthSelected",
    },
    EventDescriptor {
        topic0: abi::WinnerSelected::SIGNATURE_HASH,
        name: "WinnerSelected",
    },
    EventDescriptor {
        topic0: abi::CountCommits::SIGNATURE_HASH,
        name: "CountCommits",
    },
    EventDescriptor {
        topic0: abi::CountReveals::SIGNATURE_HASH,
        name: "CountReveals",
    },
    EventDescriptor {
        topic0: abi::ChunkCount::SIGNATURE_HASH,
        name: "ChunkCount",
    },
];

const CHEQUEBOOK_EVENTS: &[EventDescriptor] = &[EventDescriptor {
    topic0: IChequebookFactory::SimpleSwapDeployed::SIGNATURE_HASH,
    name: "SimpleSwapDeployed",
}];

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

const STORAGE_PRICE_EVENTS: &[EventDescriptor] = &[EventDescriptor {
    topic0: IStoragePriceOracle::PriceUpdate::SIGNATURE_HASH,
    name: "PriceUpdate",
}];

/// Build the watched-contract registry for `network` from the canonical nectar
/// address book.
///
/// The addresses and start blocks come straight from
/// `nectar_contracts::{mainnet,testnet}`; this function only pairs each with its
/// [`ContractId`] and event descriptor set. The result is the configuration the
/// [`crate::indexer::ContractIndexer`] watches.
pub fn registry(network: Network) -> Vec<WatchedContract> {
    use nectar_contracts::{mainnet, testnet};

    macro_rules! pick {
        ($c:ident) => {
            match network {
                Network::Mainnet => mainnet::$c,
                Network::Testnet => testnet::$c,
            }
        };
    }

    let postage = pick!(POSTAGE_STAMP);
    let staking = pick!(STAKING);
    let redistribution = pick!(REDISTRIBUTION);
    let chequebook = pick!(CHEQUEBOOK_FACTORY);
    let swap = pick!(SWAP_PRICE_ORACLE);
    let storage_price = pick!(STORAGE_PRICE_ORACLE);

    vec![
        WatchedContract {
            id: ContractId::Postage,
            address: postage.address,
            start_block: postage.block,
            events: POSTAGE_EVENTS,
        },
        WatchedContract {
            id: ContractId::Staking,
            address: staking.address,
            start_block: staking.block,
            events: STAKING_EVENTS,
        },
        WatchedContract {
            id: ContractId::Redistribution,
            address: redistribution.address,
            start_block: redistribution.block,
            events: REDISTRIBUTION_EVENTS,
        },
        WatchedContract {
            id: ContractId::ChequebookFactory,
            address: chequebook.address,
            start_block: chequebook.block,
            events: CHEQUEBOOK_EVENTS,
        },
        WatchedContract {
            id: ContractId::SwapPriceOracle,
            address: swap.address,
            start_block: swap.block,
            events: SWAP_EVENTS,
        },
        WatchedContract {
            id: ContractId::StoragePriceOracle,
            address: storage_price.address,
            start_block: storage_price.block,
            events: STORAGE_PRICE_EVENTS,
        },
    ]
}
