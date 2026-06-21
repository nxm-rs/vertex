//! The PostageStamp event ABIs, declared locally (not yet shipped by
//! `nectar-contracts`) so the reducer and the price fold share one declaration.

pub(crate) mod events {
    use alloy_sol_types::sol;

    sol! {
        /// A new batch. `normalisedBalance` is the outpayment level
        /// `currentTotalOutPayment` must stay below for the batch to stay valid.
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

        /// A batch top-up, raising `normalisedBalance`.
        #[allow(missing_docs)]
        event BatchTopUp(bytes32 indexed batchId, uint256 topupAmount, uint256 normalisedBalance);

        /// A depth increase with re-normalised balance.
        #[allow(missing_docs)]
        event BatchDepthIncrease(bytes32 indexed batchId, uint8 newDepth, uint256 normalisedBalance);

        /// The per-chunk-per-block storage price; the `currentTotalOutPayment`
        /// accumulator is folded from this cadence.
        #[allow(missing_docs)]
        event PriceUpdate(uint256 price);
    }
}
