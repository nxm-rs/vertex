//! Table definitions, registry, and secondary indexes.

use super::{DbTxMut, Key, Value};

/// A named table in the database with typed key/value pairs.
pub trait Table: Send + Sync + 'static {
    /// The table name, used to create/open the table in the backend.
    const NAME: &'static str;

    /// Whether values are zstd-compressed before storage. Defaults to `true`.
    const COMPRESS_VALUES: bool = true;

    /// The key type (must implement Encode + Decode).
    type Key: Key;

    /// The value type (must implement Serialize + DeserializeOwned).
    type Value: Value;
}

/// A secondary index table mapping `IndexKey → PrimaryKey`.
///
/// The `Table<Value: Key>` superbound ensures the index's value type (which is
/// the primary table's key type) satisfies `Key` bounds at compile time.
///
/// Index keys must be unique across all primary entries. If two primary keys
/// map to the same index key, the second insert silently overwrites the first
/// index entry, orphaning the old primary record. Uniqueness enforcement is
/// the caller's responsibility.
pub trait SecondaryIndex: Table<Value: Key> {
    /// The primary table this index refers to.
    type Primary: Table<Key = <Self as Table>::Value>;

    /// Extract the index key from a primary table value.
    fn extract(value: &<Self::Primary as Table>::Value) -> <Self as Table>::Key;
}

/// Registry of all tables a database should contain.
///
/// Used during database initialization to create all required tables.
pub trait Tables: Send + Sync + 'static {
    /// All table names that should exist in the database.
    const NAMES: &'static [&'static str];

    /// Initialize all tables via a write transaction.
    fn init<DB: super::Database>(db: &DB) -> Result<(), super::DatabaseError> {
        db.update(|tx| {
            for name in Self::NAMES {
                tx.ensure_table(name)?;
            }
            Ok(())
        })
    }
}

/// Define a table type with a name, key type, and value type.
///
/// # Examples
/// ```ignore
/// // Public table with compression (default)
/// table!(PeerTable, "peers", OverlayAddress, StoredPeer);
///
/// // Table with compression disabled
/// table!(RawTable, "raw", MyKey, MyValue, compressed = false);
///
/// // Private table (in tests or internal modules)
/// table!(pub(crate) MyTable, "my_table", MyKey, MyValue);
/// ```
#[macro_export]
macro_rules! table {
    ($vis:vis $name:ident, $table_name:literal, $key:ty, $value:ty, compressed = false) => {
        #[derive(Debug, Clone, Copy)]
        $vis struct $name;

        impl $crate::Table for $name {
            const NAME: &'static str = $table_name;
            const COMPRESS_VALUES: bool = false;
            type Key = $key;
            type Value = $value;
        }
    };
    ($vis:vis $name:ident, $table_name:literal, $key:ty, $value:ty) => {
        #[derive(Debug, Clone, Copy)]
        $vis struct $name;

        impl $crate::Table for $name {
            const NAME: &'static str = $table_name;
            type Key = $key;
            type Value = $value;
        }
    };
}

/// Define a secondary index table with an extraction closure.
///
/// Generates both `Table` (uncompressed, `IndexKey → PrimaryKey`) and
/// `SecondaryIndex` implementations. Index tables are always uncompressed
/// since they store small key references.
///
/// # Examples
/// ```ignore
/// index!(pub EthAddrPeerIndex, "peers_by_eth", Address, PeerTable, |peer| *peer.ethereum_address());
/// ```
#[macro_export]
macro_rules! index {
    ($vis:vis $name:ident, $table_name:literal, $index_key:ty, $primary:ty, |$val:ident| $extract:expr) => {
        $crate::table!($vis $name, $table_name, $index_key, <$primary as $crate::Table>::Key, compressed = false);

        impl $crate::SecondaryIndex for $name {
            type Primary = $primary;

            fn extract($val: &<$primary as $crate::Table>::Value) -> <Self as $crate::Table>::Key {
                $extract
            }
        }
    };
}
