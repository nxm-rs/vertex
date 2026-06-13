//! Contracts as data: the [`WatchedContract`] / [`EventDescriptor`] model and
//! the static [`registry`] built from the canonical nectar address book.
//!
//! A watched contract is a value, not a type. Adding a contract is one
//! [`ContractId`] variant plus one [`WatchedContract`] entry in [`registry`];
//! the indexer, the store, and the views are contract-count-independent.
//!
//! The addresses and deployment blocks come from `nectar_contracts::mainnet` /
//! `nectar_contracts::testnet`, so they live upstream exactly once and Vertex
//! reads them. The one exception is PostageStamp, StakeRegistry, and
//! Redistribution: nectar rev `6a0e0e3` ships the wrong addresses for these three
//! (while reusing the canonical deployment blocks), so they are pinned to the
//! `go-storage-incentives-abi@v0.9.4` ground truth in [`canonical`] and verified
//! at test time. See [`canonical`] for the reconciliation and the upstream
//! follow-up. The event ABIs nectar already ships
//! (`IStoragePriceOracle::PriceUpdate`, `IStakeRegistry::Stake*`,
//! `IChequebookFactory::SimpleSwapDeployed`, `ISwapPriceOracle::*`) are
//! referenced directly; the few ABIs nectar does not yet carry (the PostageStamp
//! batch/price events, `StakeRegistry::OverlayChanged`, and the Redistribution
//! event set) are declared in the local [`abi`] `sol!` block below.
//!
//! Follow-up: upstream the [`abi`] events to `nectar-contracts` so every event
//! ABI lives in one place (primitives belong in nectar per the repo split).

use alloy_primitives::{Address, B256, address};
use alloy_sol_types::SolEvent;
use nectar_contracts::{IChequebookFactory, IStakeRegistry, IStoragePriceOracle, ISwapPriceOracle};
use serde::{Deserialize, Serialize};
use strum::IntoStaticStr;

/// The canonical Swarm deployment for PostageStamp, StakeRegistry, and
/// Redistribution on Gnosis Chain and Sepolia.
///
/// These three addresses come from `go-storage-incentives-abi` (the ground truth
/// the live network and `bee` run against), NOT from `nectar_contracts`: nectar
/// rev `6a0e0e3` ships different addresses for these three while reusing the
/// canonical deployment blocks, an internally inconsistent (address, block)
/// pairing that would point the indexer at the wrong contracts and silently index
/// nothing for postage/staking/redistribution. The remaining contracts
/// (chequebook factory, swap price oracle, storage price oracle) agree with
/// nectar and are sourced from it.
///
/// Reconciled against `github.com/ethersphere/go-storage-incentives-abi@v0.9.4`
/// (`abi_mainnet.go` / `abi_testnet.go`). Verified by [`canonical_addresses`] at
/// test time so a future address change cannot land silently.
///
/// Follow-up: fix the three addresses upstream in `nectar-contracts` and re-pin,
/// then source all six from nectar and delete this override.
pub(crate) mod canonical {
    use super::{Address, address};

    /// `(address, start_block)` for a canonical deployment.
    pub(crate) struct Deployment {
        /// The contract address.
        pub(crate) address: Address,
        /// The deployment block; backfill starts here.
        pub(crate) block: u64,
    }

    /// Gnosis Chain mainnet deployments that diverge from nectar.
    pub(crate) mod mainnet {
        use super::{Deployment, address};

        /// PostageStamp (`MainnetPostageStampAddress`).
        pub(crate) const POSTAGE_STAMP: Deployment = Deployment {
            address: address!("45a1502382541Cd610CC9068e88727426b696293"),
            block: 31_305_656,
        };

        /// StakeRegistry (`MainnetStakingAddress`).
        pub(crate) const STAKING: Deployment = Deployment {
            address: address!("da2a16EE889E7F04980A8d597b48c8D51B9518F4"),
            block: 40_430_237,
        };

        /// Redistribution (`MainnetRedistributionAddress`).
        pub(crate) const REDISTRIBUTION: Deployment = Deployment {
            address: address!("5069cdfB3D9E56d23B1cAeE83CE6109A7E4fd62d"),
            block: 41_105_199,
        };
    }

    /// Sepolia testnet deployments that diverge from nectar.
    pub(crate) mod testnet {
        use super::{Deployment, address};

        /// PostageStamp (`TestnetPostageStampAddress`).
        pub(crate) const POSTAGE_STAMP: Deployment = Deployment {
            address: address!("cdfdC3752caaA826fE62531E0000C40546eC56A6"),
            block: 6_596_277,
        };

        /// StakeRegistry (`TestnetStakingAddress`).
        pub(crate) const STAKING: Deployment = Deployment {
            address: address!("EEF13Ef9eD9cDD169701eeF3cd832df298dD1bB4"),
            block: 8_262_529,
        };

        /// Redistribution (`TestnetRedistributionAddress`).
        pub(crate) const REDISTRIBUTION: Deployment = Deployment {
            address: address!("5b718E36F5Ce2F2F7e25A397040436Ce6af3e89e"),
            block: 8_646_721,
        };
    }
}

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

    // Contracts that agree with nectar are sourced from it.
    macro_rules! pick {
        ($c:ident) => {
            match network {
                Network::Mainnet => mainnet::$c,
                Network::Testnet => testnet::$c,
            }
        };
    }

    // PostageStamp/StakeRegistry/Redistribution are pinned to the canonical
    // go-storage-incentives-abi ground truth, NOT nectar (which ships the wrong
    // addresses for these three). See [`canonical`].
    macro_rules! pick_canonical {
        ($c:ident) => {
            match network {
                Network::Mainnet => canonical::mainnet::$c,
                Network::Testnet => canonical::testnet::$c,
            }
        };
    }

    let postage = pick_canonical!(POSTAGE_STAMP);
    let staking = pick_canonical!(STAKING);
    let redistribution = pick_canonical!(REDISTRIBUTION);
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

#[cfg(test)]
mod canonical_tests {
    use super::*;

    /// Look up a contract in the built registry by id.
    fn watched(network: Network, id: ContractId) -> WatchedContract {
        registry(network)
            .into_iter()
            .find(|c| c.id == id)
            .expect("contract in registry")
    }

    /// The registry must watch the canonical Swarm deployment addresses, NOT the
    /// (wrong) nectar addresses for postage/staking/redistribution.
    ///
    /// These constants are the `go-storage-incentives-abi@v0.9.4` ground truth
    /// the live network and `bee` run against. If a future nectar re-pin or a
    /// registry edit changes any of these, this test fails rather than silently
    /// pointing the indexer at the wrong contract.
    #[test]
    fn canonical_addresses() {
        // Mainnet ground truth (abi_mainnet.go).
        assert_eq!(
            watched(Network::Mainnet, ContractId::Postage).address,
            address!("45a1502382541Cd610CC9068e88727426b696293")
        );
        assert_eq!(
            watched(Network::Mainnet, ContractId::Postage).start_block,
            31_305_656
        );
        assert_eq!(
            watched(Network::Mainnet, ContractId::Staking).address,
            address!("da2a16EE889E7F04980A8d597b48c8D51B9518F4")
        );
        assert_eq!(
            watched(Network::Mainnet, ContractId::Staking).start_block,
            40_430_237
        );
        assert_eq!(
            watched(Network::Mainnet, ContractId::Redistribution).address,
            address!("5069cdfB3D9E56d23B1cAeE83CE6109A7E4fd62d")
        );
        assert_eq!(
            watched(Network::Mainnet, ContractId::Redistribution).start_block,
            41_105_199
        );

        // Testnet ground truth (abi_testnet.go).
        assert_eq!(
            watched(Network::Testnet, ContractId::Postage).address,
            address!("cdfdC3752caaA826fE62531E0000C40546eC56A6")
        );
        assert_eq!(
            watched(Network::Testnet, ContractId::Staking).address,
            address!("EEF13Ef9eD9cDD169701eeF3cd832df298dD1bB4")
        );
        assert_eq!(
            watched(Network::Testnet, ContractId::Redistribution).address,
            address!("5b718E36F5Ce2F2F7e25A397040436Ce6af3e89e")
        );
    }

    /// The contracts that DO agree with nectar must still be sourced from nectar,
    /// so a nectar re-pin keeps them in sync.
    #[test]
    fn nectar_sourced_contracts_match_nectar() {
        use nectar_contracts::{mainnet, testnet};

        assert_eq!(
            watched(Network::Mainnet, ContractId::ChequebookFactory).address,
            mainnet::CHEQUEBOOK_FACTORY.address
        );
        assert_eq!(
            watched(Network::Mainnet, ContractId::SwapPriceOracle).address,
            mainnet::SWAP_PRICE_ORACLE.address
        );
        assert_eq!(
            watched(Network::Mainnet, ContractId::StoragePriceOracle).address,
            mainnet::STORAGE_PRICE_ORACLE.address
        );
        assert_eq!(
            watched(Network::Testnet, ContractId::ChequebookFactory).address,
            testnet::CHEQUEBOOK_FACTORY.address
        );
    }

    /// The locally-declared event ABIs must match the canonical on-chain
    /// signatures byte-for-byte.
    ///
    /// Each event's `SIGNATURE_HASH` is `keccak256(signature_string)`. Asserting
    /// it against the canonical signature catches ANY drift in the local `sol!`
    /// field types or order (a drift changes the hash), so a future edit that
    /// silently breaks decode-on-read (degenerating a view to empty /
    /// always-valid) fails CI instead. The strings are the canonical event
    /// signatures from `go-storage-incentives-abi` / the deployed contracts.
    #[test]
    fn abi_signatures_match_canonical() {
        use alloy_primitives::keccak256;

        fn sig(s: &str) -> B256 {
            keccak256(s.as_bytes())
        }

        // PostageStamp.
        assert_eq!(
            abi::BatchCreated::SIGNATURE_HASH,
            sig("BatchCreated(bytes32,uint256,uint256,address,uint8,uint8,bool)")
        );
        assert_eq!(
            abi::BatchTopUp::SIGNATURE_HASH,
            sig("BatchTopUp(bytes32,uint256,uint256)")
        );
        assert_eq!(
            abi::BatchDepthIncrease::SIGNATURE_HASH,
            sig("BatchDepthIncrease(bytes32,uint8,uint256)")
        );
        assert_eq!(
            abi::PriceUpdate::SIGNATURE_HASH,
            sig("PriceUpdate(uint256)")
        );

        // StakeRegistry: the one event nectar lacks.
        assert_eq!(
            abi::OverlayChanged::SIGNATURE_HASH,
            sig("OverlayChanged(address,bytes32)")
        );

        // Redistribution.
        assert_eq!(
            abi::Committed::SIGNATURE_HASH,
            sig("Committed(uint256,bytes32,uint8)")
        );
        assert_eq!(
            abi::Revealed::SIGNATURE_HASH,
            sig("Revealed(uint256,bytes32,uint256,uint256,bytes32,uint8)")
        );
        assert_eq!(
            abi::CurrentRevealAnchor::SIGNATURE_HASH,
            sig("CurrentRevealAnchor(uint256,bytes32)")
        );
    }

    /// Every watched address is distinct, so no two contracts cross-file.
    #[test]
    fn all_addresses_distinct() {
        for network in [Network::Mainnet, Network::Testnet] {
            let mut addrs: Vec<Address> = registry(network).iter().map(|c| c.address).collect();
            let total = addrs.len();
            addrs.sort();
            addrs.dedup();
            assert_eq!(
                addrs.len(),
                total,
                "duplicate watched address on {network:?}"
            );
        }
    }
}
