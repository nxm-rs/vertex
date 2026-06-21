//! Lazy reads over the verbatim PostageStamp event stream: the price cadence is
//! folded on demand so a block-clock consumer can compute the head
//! `currentTotalOutPayment` without the indexer keeping a time-derived
//! projection.

use alloy_sol_types::SolEvent;
use vertex_chain_index_framework::events_of;
use vertex_storage::{Database, DatabaseError};

use crate::index::TAG_POSTAGE;
use crate::index::abi::events;

/// The PostageStamp `currentTotalOutPayment` at `at_block`, folded from the
/// recorded `PriceUpdate` cadence: the running sum of each interval's price times
/// its block span, up to `at_block`.
pub fn total_out_payment_at<DB: Database>(db: &DB, at_block: u64) -> Result<u128, DatabaseError> {
    let mut accumulated: u128 = 0;
    let mut last_price: u128 = 0;
    let mut last_block: u64 = 0;

    for (key, ev) in events_of(db, TAG_POSTAGE)? {
        if key.block > at_block {
            break;
        }
        if ev.topic0 != events::PriceUpdate::SIGNATURE_HASH {
            continue;
        }
        let Ok(e) = events::PriceUpdate::decode_log_data(&ev.log_data()) else {
            continue;
        };
        let Ok(price) = u128::try_from(e.price) else {
            continue;
        };
        // An interval that overflows the accumulator is decode-implausible; skip
        // it rather than saturate, since saturation would expire every batch.
        let Some(next) =
            accrued(last_price, last_block, key.block).and_then(|d| accumulated.checked_add(d))
        else {
            continue;
        };
        accumulated = next;
        last_price = price;
        last_block = key.block;
    }

    let tail = accrued(last_price, last_block, at_block)
        .and_then(|d| accumulated.checked_add(d))
        .unwrap_or(accumulated);
    Ok(tail)
}

/// `price * (to - from)`, zero when `to < from`, `None` on overflow.
fn accrued(price: u128, from: u64, to: u64) -> Option<u128> {
    let span = to.saturating_sub(from) as u128;
    price.checked_mul(span)
}
