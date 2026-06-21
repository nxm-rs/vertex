//! Swap views: the two settlement scalars by last-write-wins walk over the
//! oracle's stored rows via [`last_event`].

use alloy_primitives::U256;
use alloy_sol_types::SolEvent;
use nectar_contracts::ISwapPriceOracle;
use vertex_chain_index_framework::last_event;
use vertex_storage::{Database, DatabaseError};

use crate::index::register::TAG_SWAP_ORACLE;

/// The latest swap exchange rate (`PriceUpdate`), if ever set.
pub fn exchange_rate<DB: Database>(db: &DB) -> Result<Option<U256>, DatabaseError> {
    last_event(db, TAG_SWAP_ORACLE, |ev| {
        if ev.topic0 != ISwapPriceOracle::PriceUpdate::SIGNATURE_HASH {
            return None;
        }
        ISwapPriceOracle::PriceUpdate::decode_log_data(&ev.log_data())
            .ok()
            .map(|e| e.price)
    })
}

/// The latest cheque value deduction (`ChequeValueDeductionUpdate`), if ever set.
pub fn cheque_value_deduction<DB: Database>(db: &DB) -> Result<Option<U256>, DatabaseError> {
    last_event(db, TAG_SWAP_ORACLE, |ev| {
        if ev.topic0 != ISwapPriceOracle::ChequeValueDeductionUpdate::SIGNATURE_HASH {
            return None;
        }
        ISwapPriceOracle::ChequeValueDeductionUpdate::decode_log_data(&ev.log_data())
            .ok()
            .map(|e| e.chequeValueDeduction)
    })
}
