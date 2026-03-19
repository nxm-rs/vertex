//! Core storage traits: codecs, database, transactions, and cursors.

#![allow(clippy::type_complexity)]

use std::fmt::Debug;

use serde::{Deserialize, Serialize};

use super::{DatabaseError, SecondaryIndex, Table};

/// Encode a key for storage (keys are encoded, values are serialized).
pub trait Encode: Send + Sync + Sized + Debug {
    /// Encoded output type.
    type Encoded: AsRef<[u8]> + Into<Vec<u8>> + Send + Sync + Ord + Debug;

    /// Encode data going into the database.
    fn encode(self) -> Self::Encoded;
}

/// Decode a key read from storage.
pub trait Decode: Send + Sync + Sized + Debug {
    /// Decode data coming from the database.
    fn decode(value: &[u8]) -> Result<Self, DatabaseError>;

    /// Decode owned data coming from the database.
    fn decode_owned(value: Vec<u8>) -> Result<Self, DatabaseError> {
        Self::decode(&value)
    }
}

/// Implement `Encode` and `Decode` for a fixed-size byte type.
///
/// Requires `$ty: Into<[u8; $n]> + From<[u8; $n]>`.
#[macro_export]
macro_rules! impl_fixed_codec {
    ($ty:ty, $n:literal) => {
        impl $crate::Encode for $ty {
            type Encoded = [u8; $n];
            fn encode(self) -> Self::Encoded {
                self.into()
            }
        }
        impl $crate::Decode for $ty {
            fn decode(value: &[u8]) -> Result<Self, $crate::DatabaseError> {
                let bytes: [u8; $n] = value
                    .try_into()
                    .map_err(|_| $crate::DatabaseError::Decode)?;
                Ok(Self::from(bytes))
            }
        }
    };
}

/// Blanket trait for database keys: must be encodable, decodable, and ordered.
pub trait Key: Encode + Decode + Ord + Clone + Serialize + for<'a> Deserialize<'a> {}
impl<T> Key for T where T: Encode + Decode + Ord + Clone + Serialize + for<'a> Deserialize<'a> {}

/// Blanket trait for database values: must be serializable and deserializable.
pub trait Value: Serialize + serde::de::DeserializeOwned + Send + Sync + Sized + Debug {}
impl<T> Value for T where T: Serialize + serde::de::DeserializeOwned + Send + Sync + Sized + Debug {}

/// A database that can open read-only and read-write transactions.
pub trait Database: Send + Sync + 'static {
    /// Read-only transaction type.
    type TX: DbTx;

    /// Read-write transaction type.
    type TXMut: DbTxMut;

    /// Open a read-only transaction.
    fn tx(&self) -> Result<Self::TX, DatabaseError>;

    /// Open a read-write transaction.
    fn tx_mut(&self) -> Result<Self::TXMut, DatabaseError>;

    /// Execute a read-only closure within a transaction.
    fn view<F, R>(&self, f: F) -> Result<R, DatabaseError>
    where
        F: FnOnce(&Self::TX) -> Result<R, DatabaseError>,
    {
        let tx = self.tx()?;
        f(&tx)
    }

    /// Execute a read-write closure within a transaction, committing on success.
    fn update<F, R>(&self, f: F) -> Result<R, DatabaseError>
    where
        F: FnOnce(&Self::TXMut) -> Result<R, DatabaseError>,
    {
        let tx = self.tx_mut()?;
        let result = f(&tx)?;
        tx.commit()?;
        Ok(result)
    }
}

/// Read-only transaction operations.
pub trait DbTx: Send + Sync {
    /// Get a value by key from a table. Returns `None` if not found.
    fn get<T: Table>(&self, key: T::Key) -> Result<Option<T::Value>, DatabaseError>;

    /// Get all entries from a table as key/value pairs.
    fn entries<T: Table>(&self) -> Result<Vec<(T::Key, T::Value)>, DatabaseError>;

    /// Get all keys from a table without deserializing values.
    fn keys<T: Table>(&self) -> Result<Vec<T::Key>, DatabaseError> {
        Ok(self.entries::<T>()?.into_iter().map(|(k, _)| k).collect())
    }

    /// Count the number of entries in a table.
    fn count<T: Table>(&self) -> Result<usize, DatabaseError>;
}

/// Read-write transaction operations (extends DbTx).
pub trait DbTxMut: DbTx {
    /// Commit the transaction, persisting all writes.
    fn commit(self) -> Result<(), DatabaseError>;

    /// Insert or update a key/value pair in a table.
    fn put<T: Table>(&self, key: T::Key, value: T::Value) -> Result<(), DatabaseError>;

    /// Delete a key from a table. Returns `true` if the key existed.
    fn delete<T: Table>(&self, key: T::Key) -> Result<bool, DatabaseError>;

    /// Remove all entries from a table.
    fn clear<T: Table>(&self) -> Result<(), DatabaseError>;

    /// Ensure a table exists (create if needed). Used during initialization.
    fn ensure_table(&self, name: &str) -> Result<(), DatabaseError>;
}

/// Read-only cursor for iterating over a table.
pub trait DbCursorRO<T: Table>: Send + Sync {
    /// Move to the first entry. Returns `None` if empty.
    fn first(&mut self) -> Result<Option<(T::Key, T::Value)>, DatabaseError>;

    /// Move to the last entry. Returns `None` if empty.
    fn last(&mut self) -> Result<Option<(T::Key, T::Value)>, DatabaseError>;

    /// Seek to the first entry with key >= `key`.
    fn seek(&mut self, key: T::Key) -> Result<Option<(T::Key, T::Value)>, DatabaseError>;

    /// Seek to the exact key. Returns `None` if not found.
    fn seek_exact(&mut self, key: T::Key) -> Result<Option<(T::Key, T::Value)>, DatabaseError>;

    /// Move to the next entry. Returns `None` at end.
    fn next(&mut self) -> Result<Option<(T::Key, T::Value)>, DatabaseError>;

    /// Move to the previous entry. Returns `None` at start.
    fn prev(&mut self) -> Result<Option<(T::Key, T::Value)>, DatabaseError>;

    /// Get the current entry without advancing.
    fn current(&mut self) -> Result<Option<(T::Key, T::Value)>, DatabaseError>;
}

/// Read-write cursor for modifying entries during iteration.
pub trait DbCursorRW<T: Table>: DbCursorRO<T> {
    /// Insert or update the entry at the current cursor position.
    fn upsert(&mut self, key: T::Key, value: T::Value) -> Result<(), DatabaseError>;

    /// Insert a new entry. Errors if the key already exists.
    fn insert(&mut self, key: T::Key, value: T::Value) -> Result<(), DatabaseError>;

    /// Delete the entry at the current cursor position.
    fn delete_current(&mut self) -> Result<(), DatabaseError>;
}

/// Read via a secondary index (blanket-implemented for all `DbTx`).
pub trait IndexedRead: DbTx {
    /// Look up a primary table value via a secondary index key.
    fn get_via<I: SecondaryIndex>(
        &self,
        index_key: I::Key,
    ) -> Result<Option<<I::Primary as Table>::Value>, DatabaseError>;
}

impl<T: DbTx + ?Sized> IndexedRead for T {
    fn get_via<I: SecondaryIndex>(
        &self,
        index_key: I::Key,
    ) -> Result<Option<<I::Primary as Table>::Value>, DatabaseError> {
        let pk = match self.get::<I>(index_key)? {
            Some(pk) => pk,
            None => return Ok(None),
        };
        self.get::<I::Primary>(pk)
    }
}

/// Write with automatic secondary index maintenance (blanket-implemented for all `DbTxMut`).
pub trait IndexedWrite: DbTxMut {
    /// Insert or update a primary entry, maintaining the secondary index.
    ///
    /// Handles three cases: fresh insert, update with unchanged index key,
    /// and update with changed index key (stale index entry is removed).
    fn put_indexed<I: SecondaryIndex>(
        &self,
        pk: <I::Primary as Table>::Key,
        value: <I::Primary as Table>::Value,
    ) -> Result<(), DatabaseError>;

    /// Delete a primary entry and its secondary index entry.
    fn delete_indexed<I: SecondaryIndex>(
        &self,
        pk: <I::Primary as Table>::Key,
    ) -> Result<bool, DatabaseError>;

    /// Clear both the primary table and the index table.
    fn clear_indexed<I: SecondaryIndex>(&self) -> Result<(), DatabaseError>;
}

impl<T: DbTxMut + ?Sized> IndexedWrite for T {
    fn put_indexed<I: SecondaryIndex>(
        &self,
        pk: <I::Primary as Table>::Key,
        value: <I::Primary as Table>::Value,
    ) -> Result<(), DatabaseError> {
        let new_idx = I::extract(&value);

        // If an existing entry has a different index key, remove the stale index entry.
        if let Some(old_value) = self.get::<I::Primary>(pk.clone())? {
            let old_idx = I::extract(&old_value);
            if old_idx != new_idx {
                self.delete::<I>(old_idx)?;
            }
        }

        // Always write both entries to keep index self-healing.
        self.put::<I::Primary>(pk.clone(), value)?;
        self.put::<I>(new_idx, pk)?;
        Ok(())
    }

    fn delete_indexed<I: SecondaryIndex>(
        &self,
        pk: <I::Primary as Table>::Key,
    ) -> Result<bool, DatabaseError> {
        let value = match self.get::<I::Primary>(pk.clone())? {
            Some(v) => v,
            None => return Ok(false),
        };
        let idx = I::extract(&value);
        self.delete::<I::Primary>(pk)?;
        self.delete::<I>(idx)?;
        Ok(true)
    }

    fn clear_indexed<I: SecondaryIndex>(&self) -> Result<(), DatabaseError> {
        self.clear::<I::Primary>()?;
        self.clear::<I>()?;
        Ok(())
    }
}
