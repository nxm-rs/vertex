//! Swap view: the two settlement scalars, read at the decision point.
//!
//! `exchange_rate()` / `cheque_value_deduction()` take the last `PriceUpdate` /
//! `ChequeValueDeductionUpdate` from the swap oracle rows in position order. Two
//! scalars, polled-and-cached by the consumer. A backward walk over the
//! contract's rows (last-write-wins by position) yields each.

use alloy_primitives::U256;
use alloy_sol_types::SolEvent;
use nectar_contracts::ISwapPriceOracle;
use vertex_storage::{Database, DatabaseError};

use crate::projection::last_event;
use crate::registry::ContractId;

/// The latest swap exchange rate (`PriceUpdate`), if ever set.
pub fn exchange_rate<DB: Database>(db: &DB) -> Result<Option<U256>, DatabaseError> {
    last_event(db, ContractId::SwapPriceOracle, |ev| {
        (ev.topic0 == ISwapPriceOracle::PriceUpdate::SIGNATURE_HASH)
            .then(|| {
                ISwapPriceOracle::PriceUpdate::decode_log_data(&ev.log_data())
                    .ok()
                    .map(|e| e.price)
            })
            .flatten()
    })
}

/// The latest cheque value deduction (`ChequeValueDeductionUpdate`), if ever set.
pub fn cheque_value_deduction<DB: Database>(db: &DB) -> Result<Option<U256>, DatabaseError> {
    last_event(db, ContractId::SwapPriceOracle, |ev| {
        (ev.topic0 == ISwapPriceOracle::ChequeValueDeductionUpdate::SIGNATURE_HASH)
            .then(|| {
                ISwapPriceOracle::ChequeValueDeductionUpdate::decode_log_data(&ev.log_data())
                    .ok()
                    .map(|e| e.chequeValueDeduction)
            })
            .flatten()
    })
}
