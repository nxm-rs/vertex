//! Per-entry transaction primitives: refcount bump/decrement, entry-row and
//! full-entry deletes, and shared decode/error helpers. Each runs inside a
//! caller-provided transaction so a stamped entry and its index rows commit
//! atomically.

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
/// Content-addressed: a second stamped entry of the same content shares the body;
/// the body is never rewritten while present.
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

/// Decrement the refcount of a payload, deleting the body when it reaches zero
/// (the last stamped entry referencing it).
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

/// Delete one stamped entry's index rows (`Entry`, `BatchGroup`, `Replay`) and
/// decrement the shared payload, leaving the arbiter slot and BinCounter
/// untouched. Returns whether the entry existed.
///
/// The slot is the caller's responsibility: the restamp path rewrites it,
/// [`delete_entry_in_tx`] clears it.
pub(crate) fn delete_entry_rows_in_tx<T: DbTxMut>(
    tx: &T,
    po: u8,
    target: &EvictTarget,
) -> Result<bool, DatabaseError> {
    let entry_key = EntryKey::new(po, target.batch, target.stamp_hash, target.addr);
    let Some(value) = tx.get::<Entry>(entry_key)? else {
        return Ok(false);
    };
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

/// Fully delete a stamped entry: index rows, payload decrement, and its arbiter
/// slot (so the slot does not pin a stale newest stamp after the entry is gone).
/// Returns whether the entry existed.
pub(crate) fn delete_entry_in_tx<T: DbTxMut>(
    tx: &T,
    overlay: &OverlayAddress,
    target: &EvictTarget,
) -> Result<bool, DatabaseError> {
    let po = target.addr.proximity(overlay).get();
    let entry_key = EntryKey::new(po, target.batch, target.stamp_hash, target.addr);
    // Slot key (batch, stampIndex) is recovered from the stored stamp bytes; the
    // stamp index is not part of the entry key.
    let slot = tx
        .get::<Entry>(entry_key)?
        .and_then(|v| decode_stamp(&v.stamp_bytes).ok())
        .map(|s| StampSlotKey::new(s.batch(), s.stamp_index()));

    let removed = delete_entry_rows_in_tx(tx, po, target)?;
    if removed && let Some(slot) = slot {
        // Clear only if the slot still points at this entry's stamp hash, so a
        // concurrent restamp's slot is not clobbered.
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

/// The big-endian timestamp at bytes 40..48 of a canonical stamp encoding, used
/// to rank stamps for an address without a full decode.
pub(crate) fn stamp_timestamp(bytes: &[u8]) -> u64 {
    bytes
        .get(40..48)
        .and_then(|s| s.try_into().ok())
        .map_or(0, u64::from_be_bytes)
}

/// Decode the shared content body (type-tagged [`AnyChunk`] bytes) for an address.
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

/// The proximity order a reserve [`Bin`] denotes: for the reserve a routing
/// [`Bin`] and the table-keying [`ProximityOrder`] are the same quantity
/// measured against the local overlay. Named to keep that crossing greppable.
///
/// [`Bin`]: nectar_primitives::Bin
/// [`ProximityOrder`]: nectar_primitives::ProximityOrder
#[inline]
pub(crate) fn po_of_reserve_bin(bin: nectar_primitives::Bin) -> u8 {
    bin.get()
}

/// Map a storer/database error onto the API's storage error, preserving the source.
pub(crate) fn storage_err<E>(err: E) -> SwarmError
where
    E: std::error::Error + Send + Sync + 'static,
{
    SwarmError::storage(err)
}
