//! Database error types.

use std::fmt;

/// Detailed error information from the database backend.
#[derive(Debug)]
pub struct DatabaseErrorInfo {
    /// Human-readable error message.
    message: String,
    /// Original error from the backend, if available.
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl fmt::Display for DatabaseErrorInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.message.is_empty()
            && let Some(source) = &self.source
        {
            return write!(f, "{source}");
        }
        f.write_str(&self.message)
    }
}

impl std::error::Error for DatabaseErrorInfo {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|e| e as &(dyn std::error::Error + 'static))
    }
}

impl DatabaseErrorInfo {
    /// Create from a message string with no source error.
    pub fn msg(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            source: None,
        }
    }

    /// Create from a source error. Display delegates to the source directly.
    pub fn from_err(err: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self {
            message: String::new(),
            source: Some(Box::new(err)),
        }
    }

    /// Create with a contextual message and a source error.
    pub fn with_source(
        message: impl Into<String>,
        err: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            message: message.into(),
            source: Some(Box::new(err)),
        }
    }
}

/// Error writing a key/value pair to a table.
#[derive(Debug, thiserror::Error)]
#[error("write error for table {table_name}: key_len={key_len}, value_len={value_len}: {info}")]
pub struct DatabaseWriteError {
    /// The table name.
    pub table_name: &'static str,
    /// Length of the key that failed to write.
    pub key_len: usize,
    /// Length of the value that failed to write.
    pub value_len: usize,
    /// Backend error info.
    pub info: DatabaseErrorInfo,
}

/// Database error type.
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum DatabaseError {
    /// Failed to open the database.
    #[error("failed to open the database: {_0}")]
    Open(DatabaseErrorInfo),

    /// Failed to create a table in the database.
    #[error("failed to create a table: {_0}")]
    CreateTable(DatabaseErrorInfo),

    /// Failed to write a value into a table.
    #[error(transparent)]
    Write(Box<DatabaseWriteError>),

    /// Failed to read a value from a table.
    #[error("failed to read from table: {_0}")]
    Read(DatabaseErrorInfo),

    /// Failed to delete a key/value pair from a table.
    #[error("database delete error: {_0}")]
    Delete(DatabaseErrorInfo),

    /// Failed to commit transaction changes.
    #[error("failed to commit transaction: {_0}")]
    Commit(DatabaseErrorInfo),

    /// Failed to initiate a transaction.
    #[error("failed to initialize transaction: {_0}")]
    InitTx(DatabaseErrorInfo),

    /// Failed to initialize a cursor.
    #[error("failed to initialize cursor: {_0}")]
    InitCursor(DatabaseErrorInfo),

    /// Failed to decode a key from a table.
    #[error("failed to decode a key from a table")]
    Decode,

    /// Failed to get database stats.
    #[error("failed to get stats: {_0}")]
    Stats(DatabaseErrorInfo),

    /// Other unspecified error.
    #[error("{_0}")]
    Other(String),
}

impl DatabaseError {
    /// Create an Open error from a message.
    pub fn open(msg: impl Into<String>) -> Self {
        Self::Open(DatabaseErrorInfo::msg(msg))
    }

    /// Create an Open error preserving the source.
    pub fn open_err(err: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Open(DatabaseErrorInfo::from_err(err))
    }

    /// Create a Read error from a message.
    pub fn read(msg: impl Into<String>) -> Self {
        Self::Read(DatabaseErrorInfo::msg(msg))
    }

    /// Create a Read error preserving the source.
    pub fn read_err(err: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Read(DatabaseErrorInfo::from_err(err))
    }

    /// Create a Write error from a message.
    pub fn write(
        table: &'static str,
        key_len: usize,
        value_len: usize,
        msg: impl Into<String>,
    ) -> Self {
        Self::Write(Box::new(DatabaseWriteError {
            table_name: table,
            key_len,
            value_len,
            info: DatabaseErrorInfo::msg(msg),
        }))
    }

    /// Create a Write error preserving the source.
    pub fn write_err(
        table: &'static str,
        key_len: usize,
        value_len: usize,
        err: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::Write(Box::new(DatabaseWriteError {
            table_name: table,
            key_len,
            value_len,
            info: DatabaseErrorInfo::from_err(err),
        }))
    }

    /// Create a Delete error from a message.
    pub fn delete(msg: impl Into<String>) -> Self {
        Self::Delete(DatabaseErrorInfo::msg(msg))
    }

    /// Create a Delete error preserving the source.
    pub fn delete_err(err: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Delete(DatabaseErrorInfo::from_err(err))
    }

    /// Create a Commit error preserving the source.
    pub fn commit_err(err: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Commit(DatabaseErrorInfo::from_err(err))
    }

    /// Create an Other error from a message.
    pub fn other(msg: impl Into<String>) -> Self {
        Self::Other(msg.into())
    }
}
