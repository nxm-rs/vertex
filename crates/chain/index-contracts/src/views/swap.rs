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

use crate::registry::ContractId;
use crate::store::events_of;

/// The latest swap exchange rate (`PriceUpdate`), if ever set.
pub fn exchange_rate<DB: Database>(db: &DB) -> Result<Option<U256>, DatabaseError> {
    last_scalar(db, ISwapPriceOracle::PriceUpdate::SIGNATURE_HASH, |data| {
        ISwapPriceOracle::PriceUpdate::decode_log_data(data)
            .ok()
            .map(|e| e.price)
    })
}

/// The latest cheque value deduction (`ChequeValueDeductionUpdate`), if ever set.
pub fn cheque_value_deduction<DB: Database>(db: &DB) -> Result<Option<U256>, DatabaseError> {
    last_scalar(
        db,
        ISwapPriceOracle::ChequeValueDeductionUpdate::SIGNATURE_HASH,
        |data| {
            ISwapPriceOracle::ChequeValueDeductionUpdate::decode_log_data(data)
                .ok()
                .map(|e| e.chequeValueDeduction)
        },
    )
}

/// Walk the swap oracle rows backward and return the first scalar of `topic0`
/// that decodes; rows are position-ordered, so the last write wins.
fn last_scalar<DB, F>(
    db: &DB,
    topic0: alloy_primitives::B256,
    decode: F,
) -> Result<Option<U256>, DatabaseError>
where
    DB: Database,
    F: Fn(&alloy_primitives::LogData) -> Option<U256>,
{
    let rows = events_of(db, ContractId::SwapPriceOracle)?;
    for (_key, ev) in rows.into_iter().rev() {
        if ev.topic0 == topic0
            && let Some(v) = decode(&ev.log_data())
        {
            return Ok(Some(v));
        }
    }
    Ok(None)
}
