use alloy::primitives::{BlockNumber, BlockTimestamp, FixedBytes, U256};

pub type BatchId = FixedBytes<32>;
mod batch;
mod stamp;
pub use batch::*;
pub use stamp::*;

#[derive(Debug)]
pub struct ChainState {
    pub block_number: BlockNumber,
    pub block_timestamp: BlockTimestamp,
    pub payment: U256,
    pub price: U256,
}
