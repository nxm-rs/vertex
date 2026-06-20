//! The per-entry transaction primitives: the refcount bump/decrement, the
//! entry-row and full-entry deletes, and the small decode and error helpers
//! they share. Every function here runs inside a caller-provided transaction so
//! a stamped entry and its index rows commit atomically.

use nectar_postage::Stamp;
use nectar_primitives::ChunkAddress;
use vertex_storage::{DatabaseError, DbTxMut};
use vertex_swarm_api::SwarmError;
use vertex_swarm_postage::{StampIndexTable, StampSlotKey};
use vertex_swarm_primitives::OverlayAddress;

use super::EvictTarget;
use super::schema::{
    BatchGroup, BatchGroupKey, Entry, EntryKey, Payload, PayloadValue, Replay, ReplayKey,
};

/// Bump the refcount of an existing payload, or insert it with refcount 1.
///
/// Content-addressed: a second stamped entry of the same content shares the body
/// and increments the refcount; the body is never rewritten while present.
pub(crate) fn bump_or_insert_payload<T: DbTxMut>(
    tx: &T,
    address: ChunkAddress,
    typed_bytes: &[u8],
) -> Result<(), DatabaseError> {
    match tx.get::<Payload>(address)? {
        Some(mut p) => {
            p.refcnt += 1;
            tx.put::<Payload>(address, p)?;
        }
        None => {
            tx.put::<Payload>(
                address,
                PayloadValue {
                    refcnt: 1,
                    typed_bytes: typed_bytes.to_vec(),
                },
            )?;
        }
    }
    Ok(())
}

/// Decrement the refcount of a payload, deleting the body when it reaches zero.
///
/// The shared body survives partial eviction: it is dropped only when the last
/// stamped entry referencing it is removed.
pub(crate) fn dec_payload<T: DbTxMut>(tx: &T, address: ChunkAddress) -> Result<(), DatabaseError> {
    if let Some(mut p) = tx.get::<Payload>(address)? {
        if p.refcnt <= 1 {
            tx.delete::<Payload>(address)?;
        } else {
            p.refcnt -= 1;
            tx.put::<Payload>(address, p)?;
        }
    }
    Ok(())
}

/// Delete the four index rows of one stamped entry (`Entry`, `BatchGroup`,
/// `Replay`) and decrement the shared payload, without touching the arbiter slot
/// or the BinCounter. Returns whether the entry existed.
///
/// Used both by the restamp path (displacing the older entry, slot rewritten by
/// the caller) and by `delete_entry_in_tx` (full removal, which also clears the
/// slot).
pub(crate) fn delete_entry_rows_in_tx<T: DbTxMut>(
    tx: &T,
    po: u8,
    target: &EvictTarget,
) -> Result<bool, DatabaseError> {
    let entry_key = EntryKey::new(po, target.batch, target.stamp_hash, target.addr);
    let Some(value) = tx.get::<Entry>(entry_key)? else {
        return Ok(false);
    };
    // Replay row, addressed by the entry's stored (bin, binid).
    tx.delete::<Replay>(ReplayKey::new(value.bin, value.binid))?;
    tx.delete::<BatchGroup>(BatchGroupKey::new(
        target.batch,
        po,
        target.addr,
        target.stamp_hash,
    ))?;
    tx.delete::<Entry>(entry_key)?;
    dec_payload(tx, target.addr)?;
    Ok(true)
}

/// Fully delete a stamped entry: its four index rows, the shared payload
/// decrement, AND its arbiter slot (so the slot does not pin a stale newest
/// stamp after the entry is gone). Returns whether the entry existed.
pub(crate) fn delete_entry_in_tx<T: DbTxMut>(
    tx: &T,
    overlay: &OverlayAddress,
    target: &EvictTarget,
) -> Result<bool, DatabaseError> {
    let po = target.addr.proximity(overlay).get();
    let entry_key = EntryKey::new(po, target.batch, target.stamp_hash, target.addr);
    // Read the entry's stamp to recover its slot key (batch, stampIndex) so the
    // arbiter slot can be cleared. The stamp index is not in the key, so it is
    // decoded from the stored stamp bytes.
    let slot = tx
        .get::<Entry>(entry_key)?
        .and_then(|v| decode_stamp(&v.stamp_bytes).ok())
        .map(|s| StampSlotKey::new(s.batch(), s.stamp_index()));

    let removed = delete_entry_rows_in_tx(tx, po, target)?;
    if removed && let Some(slot) = slot {
        // Clear the slot only if it still points at this entry's stamp hash,
        // so a concurrent restamp's slot is not clobbered.
        if let Some(occupant) = tx.get::<StampIndexTable>(slot)?
            && occupant.stamp_hash == target.stamp_hash
        {
            tx.delete::<StampIndexTable>(slot)?;
        }
    }
    Ok(removed)
}

/// The verdict of a put, used to adjust the in-memory size counter.
pub(crate) enum PutOutcome {
    /// A new stamped entry was added (size += 1).
    Admitted,
    /// An older entry was displaced and a new one added (size unchanged).
    Restamped,
    /// The incoming stamp was stale; nothing written (size unchanged).
    Rejected,
}

/// Decode a stamp from its canonical 113-byte encoding.
pub(crate) fn decode_stamp(bytes: &[u8]) -> Result<Stamp, nectar_postage::StampError> {
    Stamp::try_from_slice(bytes)
}

/// The big-endian timestamp embedded in a canonical stamp encoding (bytes
/// 40..48), used to pick the newest stamp for an address without a full decode.
pub(crate) fn stamp_timestamp(bytes: &[u8]) -> u64 {
    bytes
        .get(40..48)
        .and_then(|s| s.try_into().ok())
        .map_or(0, u64::from_be_bytes)
}

/// Decode the shared content body (type-tagged [`AnyChunk`] bytes) for an
/// address.
pub(crate) fn decode_body(
    address: &ChunkAddress,
    typed_bytes: &[u8],
) -> Result<nectar_primitives::AnyChunk, SwarmError> {
    nectar_primitives::AnyChunk::from_typed_bytes(address, typed_bytes).map_err(|e| {
        SwarmError::InvalidChunk {
            address: Some(*address),
            reason: format!("stored reserve payload failed to decode: {e}"),
        }
    })
}

/// The proximity order (relative to the local overlay) a reserve [`Bin`] denotes.
///
/// For the reserve a routing [`Bin`] and the [`ProximityOrder`] the Entry/
/// BatchGroup tables key on are the *same* quantity measured against the local
/// overlay (see [`ReserveStore`]); they merely have distinct nectar types because
/// one is a slot and the other a metric, and they share the `0..=MAX_PO` range.
/// This is the single, explicit crossing of that boundary: a `Bin` in, the
/// proximity order it keys on out. The byte value is identical, but routing the
/// conversion through one named helper keeps the `Bin`-vs-`ProximityOrder`
/// conflation intentional and greppable rather than an inline `bin.get()` pun.
///
/// [`Bin`]: nectar_primitives::Bin
/// [`ProximityOrder`]: nectar_primitives::ProximityOrder
/// [`ReserveStore`]: vertex_swarm_api::ReserveStore
#[inline]
pub(crate) fn po_of_reserve_bin(bin: nectar_primitives::Bin) -> u8 {
    // A `Bin` is range-validated to `0..=MAX_PO`, which is exactly the
    // `ProximityOrder` range, so the proximity order it denotes is its raw byte.
    bin.get()
}

/// Map a storer/database error onto the API's storage error, preserving the
/// source.
pub(crate) fn storage_err<E>(err: E) -> SwarmError
where
    E: std::error::Error + Send + Sync + 'static,
{
    SwarmError::storage(err)
}
