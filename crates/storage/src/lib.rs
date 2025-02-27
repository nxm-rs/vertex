use serde::{Deserialize, Serialize};
use std::fmt::Debug;

/// Trait that will transform the data to be saved in the DB in a (ideally) compressed format
pub trait Compress: Send + Sync + Sized + Debug {
    /// Compressed type.
    type Compressed: bytes::BufMut
        + AsRef<[u8]>
        + AsMut<[u8]>
        + Into<Vec<u8>>
        + Default
        + Send
        + Sync
        + Debug;

    /// If the type cannot be compressed, return its inner reference as `Some(self.as_ref())`
    fn uncompressable_ref(&self) -> Option<&[u8]> {
        None
    }

    /// Compresses data going into the database.
    fn compress(self) -> Self::Compressed {
        let mut buf = Self::Compressed::default();
        self.compress_to_buf(&mut buf);
        buf
    }

    /// Compresses data to a given buffer.
    fn compress_to_buf<B: bytes::BufMut + AsMut<[u8]>>(&self, buf: &mut B);
}

/// Trait that will transform the data to be read from the DB.
pub trait Decompress: Send + Sync + Sized + Debug {
    /// Decompresses data coming from the database.
    fn decompress(value: &[u8]) -> Result<Self, DatabaseError>;

    /// Decompresses owned data coming from the database.
    fn decompress_owned(value: Vec<u8>) -> Result<Self, DatabaseError> {
        Self::decompress(&value)
    }
}

/// Trait that will transform the data to be saved in the DB.
pub trait Encode: Send + Sync + Sized + Debug {
    /// Encoded type.
    type Encoded: AsRef<[u8]> + Into<Vec<u8>> + Send + Sync + Ord + Debug;

    /// Encodes data going into the database.
    fn encode(self) -> Self::Encoded;
}

/// Trait that will transform the data to be read from the DB.
pub trait Decode: Send + Sync + Sized + Debug {
    /// Decodes data coming from the database.
    fn decode(value: &[u8]) -> Result<Self, DatabaseError>;

    /// Decodes owned data coming from the database.
    fn decode_owned(value: Vec<u8>) -> Result<Self, DatabaseError> {
        Self::decode(&value)
    }
}

/// Generic trait that enforces the database key to implement [`Encode`] and [`Decode`].
pub trait Key: Encode + Decode + Ord + Clone + Serialize + for<'a> Deserialize<'a> {}

impl<T> Key for T where T: Encode + Decode + Ord + Clone + Serialize + for<'a> Deserialize<'a> {}

/// Generic trait that enforces the database value to implement [`Compress`] and [`Decompress`].
pub trait Value: Compress + Decompress + Serialize {}

impl<T> Value for T where T: Compress + Decompress + Serialize {}

/// Database error type.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DatabaseError {
    // /// Failed to open the database.
    // #[error("failed to open the database: {_0}")]
    // Open(DatabaseErrorInfo),
    // /// Failed to create a table in the database.
    // #[error("failed to create a table: {_0}")]
    // CreateTable(DatabaseErrorInfo),
    // /// Failed to write a value into a table.
    // #[error(transparent)]
    // Write(Box<DatabaseWriteError>),
    // /// Failed to read a value from a table.
    // #[error("failed to read a value from a database table: {_0}")]
    // Read(DatabaseErrorInfo),
    // /// Failed to delete a `(key, value)` pair from a table.
    // #[error("database delete error code: {_0}")]
    // Delete(DatabaseErrorInfo),
    // /// Failed to commit transaction changes into the database.
    // #[error("failed to commit transaction changes: {_0}")]
    // Commit(DatabaseErrorInfo),
    // /// Failed to initiate a transaction.
    // #[error("failed to initialize a transaction: {_0}")]
    // InitTx(DatabaseErrorInfo),
    // /// Failed to initialize a cursor.
    // #[error("failed to initialize a cursor: {_0}")]
    // InitCursor(DatabaseErrorInfo),
    /// Failed to decode a key from a table.
    #[error("failed to decode a key from a table")]
    Decode,
    // /// Failed to get database stats.
    // #[error("failed to get stats: {_0}")]
    // Stats(DatabaseErrorInfo),
    // /// Failed to use the specified log level, as it's not available.
    // #[error("log level {_0:?} is not available")]
    // LogLevelUnavailable(LogLevel),
    /// Other unspecified error.
    #[error("{_0}")]
    Other(String),
}
