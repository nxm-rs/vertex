//! `sol!` event bindings for the PostageStamp contract.
//!
//! These are the exact event signatures from the contract ABI at
//! `deployments/mainnet/PostageStamp.json`. They are the only contract detail
//! this crate hard-codes; the fold in [`crate::indexer`] decodes logs through
//! them and the [`Indexer`](vertex_chain_index::Indexer) filter selects on their
//! `topic0` hashes.
//!
//! The PostageStamp contract on Gnosis Chain manages the postage batches that
//! authorise uploads to Swarm. Five events project into the indexer's state:
//!
//! - `BatchCreated` opens a batch with its full parameters.
//! - `BatchTopUp` raises a batch's `normalisedBalance`.
//! - `BatchDepthIncrease` raises a batch's depth (and re-normalises its balance).
//! - `PriceUpdate` sets the per-chunk-per-block storage price; the running
//!   `totalOutPayment` accumulator is reconstructed from its cadence.
//! - `Paused` records the contract pause state.

use alloy_sol_types::sol;

sol! {
    /// A new batch was created with its full parameters.
    ///
    /// `batchId` is indexed; the rest carry the batch's economic state at
    /// creation. `normalisedBalance` is the value the running
    /// `currentTotalOutPayment` line must stay below for the batch to remain
    /// valid.
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

    /// The per-chunk-per-block storage price was set.
    ///
    /// The contract folds the elapsed cost into `totalOutPayment` at each price
    /// change, then sets `lastPrice = price` and `lastUpdatedBlock = block`; the
    /// indexer reconstructs that same accumulator from the cadence of these
    /// events (see [`crate::projection::ChainState`]).
    #[allow(missing_docs)]
    event PriceUpdate(uint256 price);

    /// The contract was paused (or, on the same signature, unpaused) by
    /// `account`.
    #[allow(missing_docs)]
    event Paused(address account);
}
