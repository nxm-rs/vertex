//! Typed projections plus a fixed set of read combinators over them, and the lazy
//! fold backbone ([`fold_events`] / [`last_event`]) for reducer-less contracts. A
//! decode failure inside any read is scoped to the offending row (skipped), never
//! an error.

use vertex_storage::{Database, DatabaseError, DbTx, IndexedRead, SecondaryIndex, Table};

use crate::store::{EventKey, StoredEvent, events_of};
use crate::tag::ContractTag;

/// A typed projection: a [`Table`] a [`Reducer`](crate::reducer::Reducer)
/// maintains alongside the verbatim event store.
pub trait Projection: Table {}

/// A typed projection carried with a self-healing secondary index, so a view can
/// read it both by primary key ([`point_get`]) and in index order
/// ([`range_head`], [`get_via_index`]).
///
/// `Index::Primary` is the projection table, so the two stay in lockstep; the
/// [`secondary_index!`](crate::secondary_index) macro wires both.
pub trait IndexedProjection: Projection {
    /// The self-healing secondary index over this projection.
    type Index: SecondaryIndex<Primary = Self>;
}

/// Declare a typed [`Projection`] (a maintained [`Table`]) in one line.
///
/// Wraps the storage crate's [`table!`](vertex_storage::table) and adds the
/// [`Projection`] marker. Projections are uncompressed by default to match the
/// small fixed-size rows a reducer maintains; pass `compressed = false`
/// explicitly only to be emphatic (it is the default here).
#[macro_export]
macro_rules! projection {
    ($vis:vis $name:ident, $table_name:literal, $key:ty, $value:ty) => {
        $crate::__vertex_storage::table!($vis $name, $table_name, $key, $value);
        impl $crate::projection::Projection for $name {}
    };
}

/// Declare a self-healing secondary index over a [`Projection`] and bind it as
/// that projection's [`IndexedProjection::Index`] in one line.
///
/// Wraps the storage crate's [`index!`](vertex_storage::index) and additionally
/// records the index on the primary via [`IndexedProjection`], so [`range_head`]
/// / [`get_via_index`] can find it from the projection type alone.
#[macro_export]
macro_rules! secondary_index {
    ($vis:vis $name:ident, $table_name:literal, $index_key:ty, $primary:ty, |$val:ident| $extract:expr) => {
        $crate::__vertex_storage::index!($vis $name, $table_name, $index_key, $primary, |$val| $extract);
        impl $crate::projection::IndexedProjection for $primary {
            type Index = $name;
        }
    };
}

// ---------------------------------------------------------------------------
// Eager-projection combinators.
// ---------------------------------------------------------------------------

/// Point-read a projection row by its primary key.
pub fn point_get<P, DB>(db: &DB, key: P::Key) -> Result<Option<P::Value>, DatabaseError>
where
    P: Projection,
    DB: Database,
{
    db.view(|tx| tx.get::<P>(key))
}

/// Whether a projection holds a row at `key` (membership without the value).
pub fn contains<P, DB>(db: &DB, key: P::Key) -> Result<bool, DatabaseError>
where
    P: Projection,
    DB: Database,
{
    Ok(db.view(|tx| tx.get::<P>(key))?.is_some())
}

/// Every row in a projection, as `(key, value)` pairs.
// The `(P::Key, P::Value)` pair is the storage trait's own `entries` shape (which
// carries the same allow); it is a deliberate, readable signature here.
#[allow(clippy::type_complexity)]
pub fn list_all<P, DB>(db: &DB) -> Result<Vec<(P::Key, P::Value)>, DatabaseError>
where
    P: Projection,
    DB: Database,
{
    db.view(|tx| tx.entries::<P>())
}

/// Every projection row whose value satisfies `pred`.
///
/// The generic "list by some attribute" read (e.g. batches by owner): the
/// projection carries the attribute, so no secondary table is needed for a
/// linear filter.
#[allow(clippy::type_complexity)]
pub fn list_by<P, DB, F>(db: &DB, mut pred: F) -> Result<Vec<(P::Key, P::Value)>, DatabaseError>
where
    P: Projection,
    DB: Database,
    F: FnMut(&P::Value) -> bool,
{
    Ok(db
        .view(|tx| tx.entries::<P>())?
        .into_iter()
        .filter(|(_, v)| pred(v))
        .collect())
}

/// Read a projection row via its secondary index key, resolving through to the
/// primary value.
pub fn get_via_index<P, DB>(
    db: &DB,
    index_key: <P::Index as Table>::Key,
) -> Result<Option<P::Value>, DatabaseError>
where
    P: IndexedProjection,
    DB: Database,
{
    db.view(|tx| tx.get_via::<P::Index>(index_key))
}

/// Head of the secondary index in ascending order, bounded to `limit`, returned
/// as the primary keys it points at.
///
/// Scans the `[from ..= to]` range so the cost is the head window, not the whole
/// index.
pub fn range_head<P, DB>(
    db: &DB,
    from: <P::Index as Table>::Key,
    to: <P::Index as Table>::Key,
    limit: usize,
) -> Result<Vec<<P::Index as Table>::Value>, DatabaseError>
where
    P: IndexedProjection,
    DB: Database,
{
    db.view(|tx| {
        Ok(tx
            .range::<P::Index>(from, to)?
            .into_iter()
            .take(limit)
            .map(|(_, pk)| pk)
            .collect())
    })
}

/// Read a single-row summary projection at a fixed key (point_get specialised to
/// the summary read).
pub fn scalar<P, DB>(db: &DB, key: P::Key) -> Result<Option<P::Value>, DatabaseError>
where
    P: Projection,
    DB: Database,
{
    point_get::<P, DB>(db, key)
}

// ---------------------------------------------------------------------------
// Lazy fold backbone (reducer-less contracts).
// ---------------------------------------------------------------------------

/// Fold a contract's verbatim event stream in canonical order into an
/// accumulator; `step` decodes and folds each row, skipping a decode miss.
pub fn fold_events<DB, A, F>(
    db: &DB,
    tag: ContractTag,
    init: A,
    mut step: F,
) -> Result<A, DatabaseError>
where
    DB: Database,
    F: FnMut(&mut A, EventKey, &StoredEvent),
{
    let mut acc = init;
    for (key, ev) in events_of(db, tag)? {
        step(&mut acc, key, &ev);
    }
    Ok(acc)
}

/// Walk a contract's rows backward and return the first value `pick` yields (last
/// write in canonical order).
pub fn last_event<DB, T, F>(
    db: &DB,
    tag: ContractTag,
    mut pick: F,
) -> Result<Option<T>, DatabaseError>
where
    DB: Database,
    F: FnMut(&StoredEvent) -> Option<T>,
{
    for (_key, ev) in events_of(db, tag)?.into_iter().rev() {
        if let Some(v) = pick(&ev) {
            return Ok(Some(v));
        }
    }
    Ok(None)
}
