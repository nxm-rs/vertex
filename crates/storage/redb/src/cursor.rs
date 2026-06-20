//! Lazy, streaming read-only cursor over a redb table.
//!
//! [`RedbCursorRO`] implements [`DbCursorRO`] without materialising the table.
//! It owns the [`redb::ReadOnlyTable`], which `Arc`-pins the read snapshot via
//! its transaction guard, so the cursor (and any `Range` it builds) can outlive
//! the [`RedbReadTx::cursor`](crate::RedbReadTx::cursor) call that created it.
//!
//! redb's `Range` is a single iterator consuming from both ends of a fixed
//! range, so it cannot back a movable bidirectional cursor. We keep a live
//! forward range for `next()` (amortised O(1)) and rebuild a fresh range from
//! the cached current key for any backward or seeking move (O(log n)). The
//! current position is cached as raw key/value bytes so `current()` needs no
//! `Clone` bound on `T::Value`.

#![allow(clippy::type_complexity)]

use std::marker::PhantomData;

use redb::{Range, ReadOnlyTable};
use vertex_storage::{DatabaseError, DatabaseErrorInfo, DbCursorRO, Decode, Encode, Table};

use crate::tx::decode_value;

/// Raw encoded key/value bytes for the cursor's current entry.
struct Position {
    key: Vec<u8>,
    value: Vec<u8>,
}

/// A lazy read-only cursor over a single redb table.
///
/// Owns its [`ReadOnlyTable`] (which `Arc`-pins the read snapshot), so it may be
/// held across calls and returned from a function, outliving the originating tx.
pub struct RedbCursorRO<T: Table> {
    table: ReadOnlyTable<&'static [u8], &'static [u8]>,
    /// Live forward range driving `next()`; rebuilt by seeking/backward moves.
    forward: Option<Range<'static, &'static [u8], &'static [u8]>>,
    current: Option<Position>,
    /// Raw key the cursor logically sits at, used as the exclusive upper bound
    /// for `prev()`. On a seek miss it holds the absent seek key, so the
    /// `seek(hi)` then `prev()` range-tail pattern works even when `hi` lies
    /// beyond every stored key.
    anchor: Option<Vec<u8>>,
    _marker: PhantomData<fn() -> T>,
}

/// Map a redb storage error encountered while moving the cursor.
fn read_err(context: &str, err: redb::StorageError) -> DatabaseError {
    DatabaseError::Read(DatabaseErrorInfo::with_source(context.to_string(), err))
}

impl<T: Table> RedbCursorRO<T> {
    pub(crate) fn new(table: ReadOnlyTable<&'static [u8], &'static [u8]>) -> Self {
        Self {
            table,
            forward: None,
            current: None,
            anchor: None,
            _marker: PhantomData,
        }
    }

    fn decode_pair(key: &[u8], value: &[u8]) -> Result<(T::Key, T::Value), DatabaseError> {
        let k = T::Key::decode(key)?;
        let v = decode_value::<T>(value)?;
        Ok((k, v))
    }

    /// Cache the entry as both `current` and `anchor`, returning the decoded pair.
    fn record(
        &mut self,
        key: Vec<u8>,
        value: Vec<u8>,
    ) -> Result<(T::Key, T::Value), DatabaseError> {
        let pair = Self::decode_pair(&key, &value)?;
        self.anchor = Some(key.clone());
        self.current = Some(Position { key, value });
        Ok(pair)
    }

    /// Full-table forward range. The explicit byte-slice bound avoids the
    /// `RangeFull` type-inference ambiguity on the generic `range()` method.
    fn full_range(&self) -> Result<Range<'static, &'static [u8], &'static [u8]>, DatabaseError> {
        let empty: &[u8] = &[];
        self.table
            .range(empty..)
            .map_err(|e| read_err(&format!("cursor range {}", T::NAME), e))
    }

    /// Forward range starting at and including `from`.
    fn range_from(
        &self,
        from: &[u8],
    ) -> Result<Range<'static, &'static [u8], &'static [u8]>, DatabaseError> {
        self.table
            .range(from..)
            .map_err(|e| read_err(&format!("cursor seek {}", T::NAME), e))
    }

    /// Pull the next forward element from `self.forward`, decoding and caching it.
    fn advance_forward(&mut self) -> Result<Option<(T::Key, T::Value)>, DatabaseError> {
        let Some(range) = self.forward.as_mut() else {
            return Ok(None);
        };
        match range.next() {
            Some(entry) => {
                let (k, v) = entry.map_err(|e| read_err(&format!("cursor next {}", T::NAME), e))?;
                // Own the bytes before the access guards drop.
                let key = k.value().to_vec();
                let value = v.value().to_vec();
                drop((k, v));
                Ok(Some(self.record(key, value)?))
            }
            None => {
                // Keep the anchor so a subsequent prev() can step back from the
                // last visited key.
                self.forward = None;
                self.current = None;
                Ok(None)
            }
        }
    }
}

impl<T: Table> DbCursorRO<T> for RedbCursorRO<T> {
    fn first(&mut self) -> Result<Option<(T::Key, T::Value)>, DatabaseError> {
        self.forward = Some(self.full_range()?);
        self.advance_forward()
    }

    fn last(&mut self) -> Result<Option<(T::Key, T::Value)>, DatabaseError> {
        let mut range = self.full_range()?;
        match range.next_back() {
            Some(entry) => {
                let (k, v) = entry.map_err(|e| read_err(&format!("cursor last {}", T::NAME), e))?;
                let key = k.value().to_vec();
                let value = v.value().to_vec();
                drop((k, v));
                // No forward range at the tail; prev() rebuilds from the cached key.
                self.forward = None;
                Ok(Some(self.record(key, value)?))
            }
            None => {
                self.forward = None;
                self.current = None;
                self.anchor = None;
                Ok(None)
            }
        }
    }

    fn seek(&mut self, key: T::Key) -> Result<Option<(T::Key, T::Value)>, DatabaseError> {
        let encoded = key.encode();
        let key_bytes = encoded.as_ref();
        self.forward = Some(self.range_from(key_bytes)?);
        let found = self.advance_forward()?;
        if found.is_none() {
            // No key at or after `key`: anchor on it so a following prev() steps
            // back to the greatest key strictly below it.
            self.anchor = Some(key_bytes.to_vec());
        }
        Ok(found)
    }

    fn seek_exact(&mut self, key: T::Key) -> Result<Option<(T::Key, T::Value)>, DatabaseError> {
        let encoded = key.encode();
        let key_bytes = encoded.as_ref();
        let found = self
            .table
            .get(key_bytes)
            .map_err(|e| read_err(&format!("cursor seek_exact {}", T::NAME), e))?;
        match found {
            Some(guard) => {
                let value = guard.value().to_vec();
                drop(guard);
                let key_vec = key_bytes.to_vec();
                self.forward = Some(self.range_from(key_bytes)?);
                // Drop the just-found element so a following next() yields the
                // successor, matching seek().
                if let Some(range) = self.forward.as_mut() {
                    let _ = range.next();
                }
                Ok(Some(self.record(key_vec, value)?))
            }
            None => Ok(None),
        }
    }

    fn next(&mut self) -> Result<Option<(T::Key, T::Value)>, DatabaseError> {
        // Mid-scan: advance the live range. Otherwise rebuild it as the strict
        // successor of the anchor, or from the start when there is no anchor.
        if self.forward.is_none() {
            match &self.anchor {
                Some(anchor) => {
                    let mut range = self.range_from(anchor)?;
                    let _ = range.next();
                    self.forward = Some(range);
                }
                None => self.forward = Some(self.full_range()?),
            }
        }
        self.advance_forward()
    }

    fn prev(&mut self) -> Result<Option<(T::Key, T::Value)>, DatabaseError> {
        // Greatest key strictly less than the anchor; nothing before the start.
        let Some(upper) = self.anchor.clone() else {
            return Ok(None);
        };
        let upper_ref: &[u8] = &upper;
        let mut range = self
            .table
            .range(..upper_ref)
            .map_err(|e| read_err(&format!("cursor prev {}", T::NAME), e))?;
        match range.next_back() {
            Some(entry) => {
                let (k, v) = entry.map_err(|e| read_err(&format!("cursor prev {}", T::NAME), e))?;
                let key = k.value().to_vec();
                let value = v.value().to_vec();
                drop((k, v));
                // Direction changed; force next() to rebuild from this position.
                self.forward = None;
                Ok(Some(self.record(key, value)?))
            }
            None => {
                // Off the front: clear position so current() and further prev() report None.
                self.forward = None;
                self.current = None;
                self.anchor = None;
                Ok(None)
            }
        }
    }

    fn current(&mut self) -> Result<Option<(T::Key, T::Value)>, DatabaseError> {
        match &self.current {
            Some(pos) => Ok(Some(Self::decode_pair(&pos.key, &pos.value)?)),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use vertex_storage::{
        Database, DatabaseError, DbCursorRO, DbTxMut, Decode, Encode, Tables, table,
    };

    use crate::RedbDatabase;

    // A simple u64 -> u64 byte-ordered table.

    #[derive(
        Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
    )]
    struct K(u64);

    impl Encode for K {
        type Encoded = [u8; 8];
        fn encode(self) -> Self::Encoded {
            self.0.to_be_bytes()
        }
    }
    impl Decode for K {
        fn decode(value: &[u8]) -> Result<Self, DatabaseError> {
            let b: [u8; 8] = value.try_into().map_err(|_| DatabaseError::Decode)?;
            Ok(Self(u64::from_be_bytes(b)))
        }
    }

    table!(NumTable, "nums", K, u64);

    // A composite (Bin, u64) -> [u8; 4] table, big-endian so byte order is logical order.

    #[derive(
        Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
    )]
    struct BinSeq(u8, u64);

    impl Encode for BinSeq {
        type Encoded = [u8; 9];
        fn encode(self) -> Self::Encoded {
            let mut out = [0u8; 9];
            out[0] = self.0;
            out[1..].copy_from_slice(&self.1.to_be_bytes());
            out
        }
    }
    impl Decode for BinSeq {
        fn decode(value: &[u8]) -> Result<Self, DatabaseError> {
            let b: [u8; 9] = value.try_into().map_err(|_| DatabaseError::Decode)?;
            let mut seq = [0u8; 8];
            seq.copy_from_slice(&b[1..]);
            Ok(Self(b[0], u64::from_be_bytes(seq)))
        }
    }

    table!(BinTable, "bins", BinSeq, [u8; 4]);

    struct TestTables;
    impl Tables for TestTables {
        const NAMES: &'static [&'static str] = &["nums", "bins"];
    }

    fn setup() -> Arc<RedbDatabase> {
        let db = RedbDatabase::in_memory().unwrap();
        db.init_tables(TestTables::NAMES).unwrap();
        db.into_arc()
    }

    fn fill_nums(db: &RedbDatabase, keys: &[u64]) {
        db.update(|tx| {
            for &k in keys {
                tx.put::<NumTable>(K(k), k.saturating_mul(10))?;
            }
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn seek_existing_key() {
        let db = setup();
        fill_nums(&db, &[10, 20, 30]);
        let tx = db.tx().unwrap();
        let mut c = tx.cursor::<NumTable>().unwrap();
        assert_eq!(c.seek(K(20)).unwrap(), Some((K(20), 200)));
    }

    #[test]
    fn seek_missing_lands_on_next() {
        let db = setup();
        fill_nums(&db, &[10, 20, 30]);
        let tx = db.tx().unwrap();
        let mut c = tx.cursor::<NumTable>().unwrap();
        // 15 is absent: seek lands on the least key > 15.
        assert_eq!(c.seek(K(15)).unwrap(), Some((K(20), 200)));
    }

    #[test]
    fn seek_beyond_end_returns_none() {
        let db = setup();
        fill_nums(&db, &[10, 20, 30]);
        let tx = db.tx().unwrap();
        let mut c = tx.cursor::<NumTable>().unwrap();
        assert_eq!(c.seek(K(99)).unwrap(), None);
    }

    #[test]
    fn forward_scan_from_seek() {
        let db = setup();
        fill_nums(&db, &[10, 20, 30, 40]);
        let tx = db.tx().unwrap();
        let mut c = tx.cursor::<NumTable>().unwrap();
        assert_eq!(c.seek(K(20)).unwrap(), Some((K(20), 200)));
        assert_eq!(c.next().unwrap(), Some((K(30), 300)));
        assert_eq!(c.next().unwrap(), Some((K(40), 400)));
        assert_eq!(c.next().unwrap(), None);
    }

    #[test]
    fn first_and_last() {
        let db = setup();
        fill_nums(&db, &[10, 20, 30]);
        let tx = db.tx().unwrap();
        let mut c = tx.cursor::<NumTable>().unwrap();
        assert_eq!(c.first().unwrap(), Some((K(10), 100)));
        assert_eq!(c.last().unwrap(), Some((K(30), 300)));
    }

    #[test]
    fn seek_exact_hit_and_miss() {
        let db = setup();
        fill_nums(&db, &[10, 20, 30]);
        let tx = db.tx().unwrap();
        let mut c = tx.cursor::<NumTable>().unwrap();
        assert_eq!(c.seek_exact(K(20)).unwrap(), Some((K(20), 200)));
        // next() after a seek_exact hit yields the successor.
        assert_eq!(c.next().unwrap(), Some((K(30), 300)));
        // A miss returns None.
        assert_eq!(c.seek_exact(K(25)).unwrap(), None);
    }

    #[test]
    fn current_reflects_position() {
        let db = setup();
        fill_nums(&db, &[10, 20, 30]);
        let tx = db.tx().unwrap();
        let mut c = tx.cursor::<NumTable>().unwrap();
        assert_eq!(c.current().unwrap(), None);
        c.seek(K(20)).unwrap();
        assert_eq!(c.current().unwrap(), Some((K(20), 200)));
        c.next().unwrap();
        assert_eq!(c.current().unwrap(), Some((K(30), 300)));
    }

    #[test]
    fn last_entry_in_range_via_seek_then_prev() {
        let db = setup();
        // Two logical ranges: [10,20,30] and [100,110].
        fill_nums(&db, &[10, 20, 30, 100, 110]);
        let tx = db.tx().unwrap();
        let mut c = tx.cursor::<NumTable>().unwrap();
        // Last entry strictly below upper bound 40 is 30, not the global max 110:
        // proves the range-tail comes from seek(hi) + prev(), not a global last().
        assert_eq!(c.seek(K(40)).unwrap(), Some((K(100), 1000)));
        assert_eq!(c.prev().unwrap(), Some((K(30), 300)));
        assert_ne!(c.current().unwrap(), Some((K(110), 1100)));
    }

    #[test]
    fn seek_beyond_end_then_prev_gives_global_tail() {
        let db = setup();
        fill_nums(&db, &[10, 20, 30]);
        let tx = db.tx().unwrap();
        let mut c = tx.cursor::<NumTable>().unwrap();
        // seek miss past the end still anchors so prev() returns the greatest key.
        assert_eq!(c.seek(K(99)).unwrap(), None);
        assert_eq!(c.prev().unwrap(), Some((K(30), 300)));
    }

    #[test]
    fn direction_change_next_then_prev() {
        let db = setup();
        fill_nums(&db, &[10, 20, 30, 40]);
        let tx = db.tx().unwrap();
        let mut c = tx.cursor::<NumTable>().unwrap();
        assert_eq!(c.seek(K(20)).unwrap(), Some((K(20), 200)));
        assert_eq!(c.next().unwrap(), Some((K(30), 300)));
        // prev() returns the element before the current (30) -> 20.
        assert_eq!(c.prev().unwrap(), Some((K(20), 200)));
        // and forward again advances to 30.
        assert_eq!(c.next().unwrap(), Some((K(30), 300)));
    }

    #[test]
    fn prev_from_first_returns_none() {
        let db = setup();
        fill_nums(&db, &[10, 20, 30]);
        let tx = db.tx().unwrap();
        let mut c = tx.cursor::<NumTable>().unwrap();
        assert_eq!(c.first().unwrap(), Some((K(10), 100)));
        assert_eq!(c.prev().unwrap(), None);
        // current() is cleared after running off the front.
        assert_eq!(c.current().unwrap(), None);
    }

    #[test]
    fn empty_table() {
        let db = setup();
        let tx = db.tx().unwrap();
        let mut c = tx.cursor::<NumTable>().unwrap();
        assert_eq!(c.first().unwrap(), None);
        assert_eq!(c.last().unwrap(), None);
        assert_eq!(c.seek(K(5)).unwrap(), None);
        assert_eq!(c.next().unwrap(), None);
        assert_eq!(c.prev().unwrap(), None);
        assert_eq!(c.current().unwrap(), None);
    }

    #[test]
    fn boundary_keys() {
        let db = setup();
        fill_nums(&db, &[u64::MIN, 1, u64::MAX]);
        let tx = db.tx().unwrap();
        let mut c = tx.cursor::<NumTable>().unwrap();
        assert_eq!(c.seek(K(u64::MIN)).unwrap(), Some((K(u64::MIN), 0)));
        assert_eq!(
            c.last().unwrap(),
            Some((K(u64::MAX), u64::MAX.saturating_mul(10)))
        );
        // prev() from the maximum steps to 1.
        assert_eq!(c.prev().unwrap(), Some((K(1), 10)));
    }

    #[test]
    fn single_element_table() {
        let db = setup();
        fill_nums(&db, &[42]);
        let tx = db.tx().unwrap();
        let mut c = tx.cursor::<NumTable>().unwrap();
        assert_eq!(c.first().unwrap(), Some((K(42), 420)));
        assert_eq!(c.next().unwrap(), None);
        c.first().unwrap();
        assert_eq!(c.prev().unwrap(), None);
        assert_eq!(c.last().unwrap(), Some((K(42), 420)));
    }

    #[test]
    fn cursor_outlives_originating_call() {
        // The cursor Arc-pins the snapshot, so it survives `tx` being dropped here.
        fn make_cursor(db: &RedbDatabase) -> crate::RedbCursorRO<NumTable> {
            let tx = db.tx().unwrap();
            let mut c = tx.cursor::<NumTable>().unwrap();
            c.seek(K(10)).unwrap();
            c
        }

        let db = setup();
        fill_nums(&db, &[10, 20, 30]);
        let mut c = make_cursor(&db);
        // Still usable after the originating tx was dropped.
        assert_eq!(c.current().unwrap(), Some((K(10), 100)));
        assert_eq!(c.next().unwrap(), Some((K(20), 200)));
        assert_eq!(c.next().unwrap(), Some((K(30), 300)));
    }

    #[test]
    fn cursor_is_send_and_sync() {
        fn assert_ss<X: Send + Sync>() {}
        assert_ss::<crate::RedbCursorRO<NumTable>>();
    }

    // Composite-key bin scan and per-bin topmost-seq pattern.

    #[test]
    fn bin_scan_and_topmost_seq() {
        let db = setup();
        db.update(|tx| {
            // bin 3: seq 0,1,2 ; bin 4: seq 0,1
            for seq in 0..3u64 {
                tx.put::<BinTable>(BinSeq(3, seq), [3, seq as u8, 0, 0])?;
            }
            for seq in 0..2u64 {
                tx.put::<BinTable>(BinSeq(4, seq), [4, seq as u8, 0, 0])?;
            }
            Ok(())
        })
        .unwrap();

        let tx = db.tx().unwrap();

        // scan_bin_from(3, 1): stream (seq, addr) from seq 1, stopping at the bin boundary.
        let mut c = tx.cursor::<BinTable>().unwrap();
        let mut scanned = Vec::new();
        let mut cur = c.seek(BinSeq(3, 1)).unwrap();
        while let Some((BinSeq(bin, seq), _addr)) = cur {
            if bin != 3 {
                break;
            }
            scanned.push(seq);
            cur = c.next().unwrap();
        }
        assert_eq!(scanned, vec![1, 2]);

        // topmost seq in bin 3 = seek((4, 0)) then prev(), guarding bin == 3.
        let mut c2 = tx.cursor::<BinTable>().unwrap();
        c2.seek(BinSeq(4, 0)).unwrap();
        let top = c2.prev().unwrap();
        assert_eq!(top, Some((BinSeq(3, 2), [3, 2, 0, 0])));

        // topmost seq in the highest bin 4 = seek((5, 0)) [miss] then prev().
        let mut c3 = tx.cursor::<BinTable>().unwrap();
        assert_eq!(c3.seek(BinSeq(5, 0)).unwrap(), None);
        assert_eq!(c3.prev().unwrap(), Some((BinSeq(4, 1), [4, 1, 0, 0])));

        // empty bin 9: seek((10,0)) misses, prev() lands in bin 4 -> guard rejects.
        let mut c4 = tx.cursor::<BinTable>().unwrap();
        c4.seek(BinSeq(10, 0)).unwrap();
        let below = c4.prev().unwrap();
        assert!(matches!(below, Some((BinSeq(b, _), _)) if b != 9));
    }

    // A value type that counts deserialisations, so a test can assert the cursor
    // decodes only what it consumes, not the whole table.
    static DECODE_COUNT: AtomicUsize = AtomicUsize::new(0);

    #[derive(Debug, PartialEq, serde::Serialize)]
    struct Counted(u64);

    impl<'de> serde::Deserialize<'de> for Counted {
        fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            DECODE_COUNT.fetch_add(1, Ordering::SeqCst);
            Ok(Counted(u64::deserialize(d)?))
        }
    }

    table!(CountTable, "nums", K, Counted);

    #[test]
    fn cursor_streams_without_materialising() {
        let db = setup();
        // 1000 rows; a materialising implementation would decode all of them.
        db.update(|tx| {
            for k in 0..1000u64 {
                tx.put::<NumTable>(K(k), k)?;
            }
            Ok(())
        })
        .unwrap();

        DECODE_COUNT.store(0, Ordering::SeqCst);
        let tx = db.tx().unwrap();
        let mut c = tx.cursor::<CountTable>().unwrap();
        // Seek into the middle and consume only three entries.
        c.seek(K(500)).unwrap();
        c.next().unwrap();
        c.next().unwrap();
        // Exactly three values decoded, not 1000.
        assert_eq!(DECODE_COUNT.load(Ordering::SeqCst), 3);
    }
}
